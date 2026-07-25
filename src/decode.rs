use crate::encode::byte_planes_for;
use crate::error::{Dm2Error, Result};
use crate::format::*;
use crate::lzfse;
use crate::predict::{self, PredictMode};
use crate::verified;

pub fn decode(data: &[u8], pixels: &mut [u8], info: &mut ImageInfo) -> Result<()> {
    let (header, hdr_len) = Header::read(data)?;
    let width = header.tile_width as u32;
    let ps = header.format.pixel_size();
    let row_bytes = (width as usize)
        .checked_mul(ps)
        .ok_or(Dm2Error::BufferTooSmall)?;
    if row_bytes == 0 {
        return Err(Dm2Error::BadFormat);
    }

    // The header stores tile dimensions, not full image dimensions, so
    // the caller's output buffer is the only place we can recover the
    // image height from. Require it to be a clean multiple of one row
    // and within u32; otherwise the dimensions are inconsistent.
    if pixels.len() % row_bytes != 0 {
        return Err(Dm2Error::BufferTooSmall);
    }
    let image_height_us = pixels.len() / row_bytes;
    if image_height_us > u32::MAX as usize {
        return Err(Dm2Error::BufferTooSmall);
    }
    let image_height = image_height_us as u32;

    info.width = width;
    info.height = image_height;
    info.format = header.format;

    // Validate that the inferred dimensions don't overflow when multiplied
    // out — protects every downstream allocation sized from `info`.
    info.checked_raw_size()?;

    match header.compression {
        Compression::None => decode_none(&data[hdr_len..], pixels, info),
        Compression::Lossless => decode_tiled(&data[hdr_len..], pixels, info, &header, |tile_data, out, _w, _h| {
            let decompressed = lzfse::decompress(tile_data, out.len())?;
            if decompressed.len() != out.len() {
                return Err(Dm2Error::DecodeFailed);
            }
            out.copy_from_slice(&decompressed);
            Ok(())
        }),
        Compression::Default => decode_default(data, hdr_len, pixels, info, &header),
        Compression::Palette => decode_palette(data, hdr_len, pixels, info, &header),
    }
}

pub fn read_info(data: &[u8]) -> Result<(ImageInfo, Compression)> {
    let (header, _) = Header::read(data)?;
    // To get true image height we'd need to walk all tiles.
    // For single-tile images, tile dims = image dims.
    // For multi-tile, we can compute from file size for type 1,
    // but for compressed types we'd need to walk tiles.
    // Return tile dims as a reasonable approximation; exact dims
    // require full decode or a tile-walking pass.
    Ok((
        ImageInfo {
            width: header.tile_width as u32,
            height: header.tile_height as u32,
            format: header.format,
        },
        header.compression,
    ))
}

fn decode_none(tile_data: &[u8], pixels: &mut [u8], info: &ImageInfo) -> Result<()> {
    let expected = info.raw_size();
    if tile_data.len() < expected || pixels.len() < expected {
        return Err(Dm2Error::BufferTooSmall);
    }
    pixels[..expected].copy_from_slice(&tile_data[..expected]);
    Ok(())
}

fn decode_tiled<F>(
    data: &[u8],
    pixels: &mut [u8],
    info: &ImageInfo,
    header: &Header,
    decode_tile: F,
) -> Result<()>
where
    F: Fn(&[u8], &mut [u8], usize, usize) -> Result<()>,
{
    let w = info.width as usize;
    let tile_h = header.tile_height as usize;
    let row_bytes = info.row_bytes();
    let mut offset = 0;
    let mut pixel_row = 0usize;
    let h = info.height as usize;

    while pixel_row < h {
        if offset + 4 > data.len() {
            return Err(Dm2Error::DecodeFailed);
        }
        // Bounds checked immediately above.
        let tile_sz = u32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]) as usize;
        offset += 4;
        if offset + tile_sz > data.len() {
            return Err(Dm2Error::DecodeFailed);
        }
        let rows = tile_h.min(h - pixel_row);
        let pix_start = pixel_row * row_bytes;
        let pix_end = pix_start + rows * row_bytes;
        decode_tile(&data[offset..offset + tile_sz], &mut pixels[pix_start..pix_end], w, rows)?;
        offset += tile_sz;
        pixel_row += rows;
    }

    Ok(())
}

fn decode_default(data: &[u8], hdr_len: usize, pixels: &mut [u8], info: &ImageInfo, header: &Header) -> Result<()> {
    if info.format.is_16bit() {
        return Err(Dm2Error::BadFormat);
    }

    decode_tiled(&data[hdr_len..], pixels, info, header, |tile_data, out, w, h| {
        decode_default_tile(tile_data, out, w, h, info.format)
    })
}

// GrayA, RGB, and RGBA all use decode_default_tile_ycc (the unified
// multi-channel decoder). Only Gray8 uses this single-channel path.
fn decode_default_tile(tile_data: &[u8], pixels: &mut [u8], w: usize, h: usize, format: PixelFormat) -> Result<()> {
    if format.channels() >= 2 {
        return decode_default_tile_ycc(tile_data, pixels, w, h, format);
    }

    // min_size = h * (2*w + 1); compute with checked arithmetic so hostile
    // tile dimensions can't overflow on 32-bit targets or wrap to a small value.
    let plane = w.checked_mul(h).ok_or(Dm2Error::BadFormat)?;
    let two_planes = plane.checked_mul(2).ok_or(Dm2Error::BadFormat)?;
    let min_size = two_planes.checked_add(h).ok_or(Dm2Error::BadFormat)?;
    let buf_size = min_size.checked_add(15).ok_or(Dm2Error::BadFormat)? & !15;

    let decompressed = lzfse::decompress(tile_data, buf_size)?;
    if decompressed.len() < min_size || decompressed.len() > buf_size {
        return Err(Dm2Error::DecodeFailed);
    }
    // Caller passed `pixels[pix_start..pix_end]` sized `rows * row_bytes`,
    // i.e. exactly h*w bytes for a 1-channel format. Verify defensively.
    if pixels.len() < plane {
        return Err(Dm2Error::BufferTooSmall);
    }

    let mut cur = vec![0i16; w];
    let mut prev = vec![0i16; w];
    let mut residuals = vec![0i16; w];

    for row in 0..h {
        let mode_byte = *decompressed.get(row).ok_or(Dm2Error::DecodeFailed)?;
        let mode = PredictMode::from_u8(mode_byte).ok_or(Dm2Error::DecodeFailed)?;

        let high_plane = h + row * w;
        let low_plane = h + plane + row * w;
        let high = decompressed.get(high_plane..high_plane + w).ok_or(Dm2Error::DecodeFailed)?;
        let low = decompressed.get(low_plane..low_plane + w).ok_or(Dm2Error::DecodeFailed)?;
        for i in 0..w {
            let z = ((high[i] as u16) << 8) | (low[i] as u16);
            residuals[i] = verified::unadjust_residual(predict::zigzag_decode(z));
        }

        let prev_ref = if row > 0 { Some(prev.as_slice()) } else { None };
        predict::unpredict_row(&residuals, prev_ref, mode, &mut cur)?;

        let row_pixels = &mut pixels[row * w..(row + 1) * w];
        for i in 0..w {
            row_pixels[i] = cur[i].clamp(0, 255) as u8;
        }
        for i in 0..w {
            prev[i] = row_pixels[i] as i16;
        }
    }

    Ok(())
}

/// RGB/RGBA type 2 tile decoder.
/// Layout for alpha formats: [W*H alpha][H ycc_modes][n_color*W*H high][n_color*W*H low]
/// Layout for non-alpha:     [H ycc_modes][n_color*W*H high][n_color*W*H low]
fn decode_default_tile_ycc(tile_data: &[u8], pixels: &mut [u8], w: usize, h: usize, format: PixelFormat) -> Result<()> {
    let channels = format.channels();
    let has_alpha = channels == 2 || channels == 4;
    let k = byte_planes_for(format);
    // Checked size arithmetic — guards against overflow on hostile tile dims.
    let plane = w.checked_mul(h).ok_or(Dm2Error::BadFormat)?;
    let kw = k.checked_mul(w).ok_or(Dm2Error::BadFormat)?;
    let kw1 = kw.checked_add(1).ok_or(Dm2Error::BadFormat)?;
    let min_size = h.checked_mul(kw1).ok_or(Dm2Error::BadFormat)?;
    let buf_size = min_size.checked_add(15).ok_or(Dm2Error::BadFormat)? & !15;

    let buf = lzfse::decompress(tile_data, buf_size)?;
    if buf.len() < min_size || buf.len() > buf_size {
        return Err(Dm2Error::DecodeFailed);
    }

    let n_color: usize = if channels <= 2 { 1 } else { 3 };
    let alpha_plane_size = if has_alpha { plane } else { 0 };
    let ycc_modes_off = alpha_plane_size;
    let high_off = alpha_plane_size.checked_add(h).ok_or(Dm2Error::BadFormat)?;
    let n_color_plane = n_color.checked_mul(plane).ok_or(Dm2Error::BadFormat)?;
    let low_off = high_off.checked_add(n_color_plane).ok_or(Dm2Error::BadFormat)?;
    // Final byte we will read is at low_off + n_color_plane - 1; ensure buf covers it.
    let end = low_off.checked_add(n_color_plane).ok_or(Dm2Error::BadFormat)?;
    if buf.len() < end {
        return Err(Dm2Error::DecodeFailed);
    }

    let mut prev_y = vec![0i16; w];
    let mut prev_co = vec![0i16; w];
    let mut prev_cg = vec![0i16; w];

    for row in 0..h {
        let ycc_mode = *buf.get(ycc_modes_off + row).ok_or(Dm2Error::DecodeFailed)?;

        let mut cur_y = vec![0i16; w];
        let mut cur_co = vec![0i16; w];
        let mut cur_cg = vec![0i16; w];

        // A malformed mode byte should fail the decode rather than be silently
        // coerced to None — that would mask corruption and produce wrong pixels.
        let mode = PredictMode::from_u8(ycc_mode).ok_or(Dm2Error::DecodeFailed)?;
        for i in 0..w {
            let hi = high_off + row * n_color * w + i * n_color;
            let lo = low_off + row * n_color * w + i * n_color;
            let zz_y = ((buf[hi] as u16) << 8) | buf[lo] as u16;
            let mut res_y = predict::zigzag_decode(zz_y);
            let (mut res_co, mut res_cg) = if n_color >= 3 {
                let zz_co = ((buf[hi + 1] as u16) << 8) | buf[lo + 1] as u16;
                let zz_cg = ((buf[hi + 2] as u16) << 8) | buf[lo + 2] as u16;
                (predict::zigzag_decode(zz_co), predict::zigzag_decode(zz_cg))
            } else { (0, 0) };

            res_y = verified::unadjust_residual(res_y);
            res_co = verified::unadjust_residual(res_co);
            res_cg = verified::unadjust_residual(res_cg);

            // All additions below use the verified wrap-around add: on a
            // valid stream the values never overflow (so this is
            // equivalent to `+`), but residuals here come from attacker-
            // controlled bytes and plain `+` panics debug builds on
            // hostile input (found by tests/verified_props.rs).
            let wadd = verified::wrap_add_i16;
            match mode {
                PredictMode::None => {
                    cur_y[i] = res_y; cur_co[i] = res_co; cur_cg[i] = res_cg;
                }
                PredictMode::Left => {
                    cur_y[i] = wadd(res_y, if i == 0 { 0 } else { cur_y[i - 1] });
                    cur_co[i] = wadd(res_co, if i == 0 { 0 } else { cur_co[i - 1] });
                    cur_cg[i] = wadd(res_cg, if i == 0 { 0 } else { cur_cg[i - 1] });
                }
                PredictMode::Up => {
                    cur_y[i] = wadd(res_y, prev_y[i]);
                    cur_co[i] = wadd(res_co, prev_co[i]);
                    cur_cg[i] = wadd(res_cg, prev_cg[i]);
                }
                PredictMode::UpLeft => {
                    // 2-way Paeth: the selection is computed once on the Y
                    // channel and applied to all three YCoCg channels.
                    let (py, pco, pcg) = if i == 0 {
                        (prev_y[0], prev_co[0], prev_cg[0])
                    } else {
                        let p = prev_y[i] as i32 + cur_y[i-1] as i32 - prev_y[i-1] as i32;
                        let pa = (p - cur_y[i-1] as i32).unsigned_abs();
                        let pb = (p - prev_y[i] as i32).unsigned_abs();
                        if pb < pa {
                            (prev_y[i], prev_co[i], prev_cg[i])
                        } else {
                            (cur_y[i-1], cur_co[i-1], cur_cg[i-1])
                        }
                    };
                    cur_y[i] = wadd(res_y, py);
                    cur_co[i] = wadd(res_co, pco);
                    cur_cg[i] = wadd(res_cg, pcg);
                }
                PredictMode::Mean => {
                    if i == 0 {
                        cur_y[i] = wadd(res_y, prev_y[i]);
                        cur_co[i] = wadd(res_co, prev_co[i]);
                        cur_cg[i] = wadd(res_cg, prev_cg[i]);
                    } else {
                        let mut sy = cur_y[i-1] as i32 + prev_y[i] as i32 + 1; if sy < 0 { sy += 1; }
                        let mut sco = cur_co[i-1] as i32 + prev_co[i] as i32 + 1; if sco < 0 { sco += 1; }
                        let mut scg = cur_cg[i-1] as i32 + prev_cg[i] as i32 + 1; if scg < 0 { scg += 1; }
                        cur_y[i] = wadd(res_y, (sy >> 1) as i16);
                        cur_co[i] = wadd(res_co, (sco >> 1) as i16);
                        cur_cg[i] = wadd(res_cg, (scg >> 1) as i16);
                    }
                }
            }
        }

        // Standard inverse YCoCg (no color un-decrement — the un-adjustment
        // above already recovers original Co/Cg values)
        let ps = format.pixel_size();
        let row_pixels = &mut pixels[row * w * ps..(row + 1) * w * ps];

        match format {
            // The inverse YCoCg math is done in i32: C integer promotion
            // means Apple's decoder computes these in `int` too, and with
            // hostile residuals the intermediate values can exceed i16
            // (plain i16 `+`/`-` here panics debug builds — found by
            // tests/verified_props.rs). i32 cannot overflow: |inputs| ≤
            // 32768 and the expression depth is small.
            PixelFormat::Rgba8 => {
                for i in 0..w {
                    let co = cur_co[i] as i32;
                    let cg = cur_cg[i] as i32;
                    let t = cur_y[i] as i32 - cg / 2;
                    let g = cg + t;
                    let b = t - co / 2;
                    let r = co + b;
                    row_pixels[i * 4] = r.clamp(0, 255) as u8;
                    row_pixels[i * 4 + 1] = g.clamp(0, 255) as u8;
                    row_pixels[i * 4 + 2] = b.clamp(0, 255) as u8;
                    row_pixels[i * 4 + 3] = buf[row * w + i];
                }
            }
            PixelFormat::Rgb8 => {
                for i in 0..w {
                    let co = cur_co[i] as i32;
                    let cg = cur_cg[i] as i32;
                    let t = cur_y[i] as i32 - cg / 2;
                    let g = cg + t;
                    let b = t - co / 2;
                    let r = co + b;
                    row_pixels[i * 3] = r.clamp(0, 255) as u8;
                    row_pixels[i * 3 + 1] = g.clamp(0, 255) as u8;
                    row_pixels[i * 3 + 2] = b.clamp(0, 255) as u8;
                }
            }
            PixelFormat::GrayA8 => {
                for i in 0..w {
                    row_pixels[i * 2] = cur_y[i].clamp(0, 255) as u8;
                    row_pixels[i * 2 + 1] = buf[row * w + i];
                }
            }
            // 1-channel and 16-bit formats are routed away before reaching
            // here; reject explicitly rather than panic if a future caller
            // breaks the dispatch invariant.
            _ => return Err(Dm2Error::BadFormat),
        }

        prev_y = cur_y;
        prev_co = cur_co;
        prev_cg = cur_cg;
    }

    Ok(())
}



fn decode_palette(data: &[u8], hdr_len: usize, pixels: &mut [u8], info: &ImageInfo, header: &Header) -> Result<()> {
    let palette = header.palette.as_ref().ok_or(Dm2Error::DecodeFailed)?;
    let has_tile_alpha = header.palette_bpe == 3;

    decode_tiled(&data[hdr_len..], pixels, info, header, |tile_data, out, w, h| {
        let npix = w.checked_mul(h).ok_or(Dm2Error::BadFormat)?;
        let raw_size = if has_tile_alpha {
            npix.checked_mul(2).ok_or(Dm2Error::BadFormat)?
        } else {
            npix
        };
        let padded = raw_size.checked_add(15).ok_or(Dm2Error::BadFormat)? & !15;
        let tile = lzfse::decompress(tile_data, padded)?;
        if tile.len() < raw_size || tile.len() > padded {
            return Err(Dm2Error::DecodeFailed);
        }
        let (idx_off, alpha_off) = if has_tile_alpha { (npix, 0) } else { (0, 0) };
        for i in 0..npix {
            let idx = tile[idx_off + i] as usize;
            if idx >= palette.len() {
                return Err(Dm2Error::DecodeFailed);
            }
            let c = palette[idx];
            out[i * 4] = c[0];
            out[i * 4 + 1] = c[1];
            out[i * 4 + 2] = c[2];
            out[i * 4 + 3] = if has_tile_alpha { tile[alpha_off + i] } else { c[3] };
        }
        Ok(())
    })
}
