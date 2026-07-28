use crate::color::f32_to_f16_bits;
use crate::error::{Dm2Error, Result};
use crate::format::byte_planes_for;
use crate::format::*;
use crate::lzfse;
use crate::predict::{self, PredictMode};

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
        // Compare by subtraction, not `offset + tile_sz > data.len()`:
        // `tile_sz` is a full attacker-controlled u32, so on a 32-bit target
        // the sum would wrap past this guard and the slice below would panic.
        if tile_sz > data.len() - offset {
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
        // Apple requires param 9..=12 for 16-bit formats — it selects the
        // fixed-point scale (2^(param-1)) of the stored channel codes.
        if !(9..=12).contains(&header.param) {
            return Err(Dm2Error::BadFormat);
        }
    }

    let quality = header.quality;
    let param = header.param;
    decode_tiled(&data[hdr_len..], pixels, info, header, |tile_data, out, w, h| {
        decode_default_tile(tile_data, out, w, h, info.format, quality, param)
    })
}

// GrayA, RGB, and RGBA all use decode_default_tile_ycc (the unified
// multi-channel decoder). Only Gray8 uses this single-channel path.
fn decode_default_tile(tile_data: &[u8], pixels: &mut [u8], w: usize, h: usize, format: PixelFormat, quality: u8, param: u8) -> Result<()> {
    if format.channels() >= 2 {
        return decode_default_tile_ycc(tile_data, pixels, w, h, format, quality, param);
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
    // Caller passed `pixels[pix_start..pix_end]` sized `rows * row_bytes`.
    // Verify defensively.
    let ps = format.pixel_size();
    if pixels.len() < plane.checked_mul(ps).ok_or(Dm2Error::BadFormat)? {
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
            let mut res = predict::zigzag_decode(z);
            if res < 0 { res += 1; }
            residuals[i] = res;
        }

        let prev_ref = if row > 0 { Some(prev.as_slice()) } else { None };
        predict::unpredict_row(&residuals, prev_ref, mode, &mut cur)?;

        let row_pixels = &mut pixels[row * w * ps..(row + 1) * w * ps];
        match format {
            PixelFormat::Gray8 => {
                // Output clamps to u8, and the CLAMPED value feeds the next
                // row's prediction (matches Apple's 8-bit gray decoder).
                for i in 0..w {
                    row_pixels[i] = cur[i].clamp(0, 255) as u8;
                    prev[i] = row_pixels[i] as i16;
                }
            }
            PixelFormat::Gray16 => {
                // Same plane layout as Gray8; the reconstructed integers are
                // fixed-point codes of half-float values (scale 2^(param-1)),
                // and the UNCLAMPED code feeds the next row's prediction —
                // verified byte-identical to vImageDeepmap2Decode across the
                // full param 9..=12 × quality grid. Quality has no effect
                // (no chroma planes), same as 8-bit gray.
                let scale = (1u32 << (param - 1)) as f32;
                for i in 0..w {
                    let half = f32_to_f16_bits(cur[i] as f32 / scale);
                    row_pixels[i * 2..i * 2 + 2].copy_from_slice(&half.to_le_bytes());
                    prev[i] = cur[i];
                }
            }
            // Multi-channel formats were dispatched to the YCC decoder above.
            _ => return Err(Dm2Error::BadFormat),
        }
    }

    Ok(())
}

/// RGB/RGBA type 2 tile decoder.
/// Layout for alpha formats: [W*H alpha][H ycc_modes][n_color*W*H high][n_color*W*H low]
/// Layout for non-alpha:     [H ycc_modes][n_color*W*H high][n_color*W*H low]
fn decode_default_tile_ycc(tile_data: &[u8], pixels: &mut [u8], w: usize, h: usize, format: PixelFormat, quality: u8, param: u8) -> Result<()> {
    // Apple stores Co/Cg at HALF scale when the header `quality` is non-zero, full scale when it
    // is 0 — this halving IS the documented quality=1 "maxerr=1 on R/G" loss. It is keyed on the
    // quality byte, NOT param: vImage cross-validation shows param has no effect on 8-bit streams
    // (q1/p0 and q1/p10 encode byte-identically; q0/p10 streams are full-scale), and the same rule
    // holds for RGBA16. An earlier param-based rule here only worked because every real .car
    // rendition ships with (quality=1, param=10). For 16-bit formats param instead selects the
    // fixed-point scale of the channel codes (see the Rgba16 output arm below).
    let chroma_scale: i16 = if quality != 0 { 2 } else { 1 };
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

            if res_y < 0 { res_y += 1; }
            if res_co < 0 { res_co += 1; }
            if res_cg < 0 { res_cg += 1; }

            // All accumulation WRAPS at i16, matching Apple's 16-bit-lane
            // arithmetic. Real 16-bit streams do exceed i16 (the encoder
            // itself wraps out-of-range fixed-point codes), so wrapping is
            // load-bearing semantics — not just overflow hygiene — verified
            // against vImageDeepmap2Decode on codes crossing ±32768.
            match mode {
                PredictMode::None => {
                    cur_y[i] = res_y; cur_co[i] = res_co; cur_cg[i] = res_cg;
                }
                PredictMode::Left => {
                    cur_y[i] = res_y.wrapping_add(if i == 0 { 0 } else { cur_y[i - 1] });
                    cur_co[i] = res_co.wrapping_add(if i == 0 { 0 } else { cur_co[i - 1] });
                    cur_cg[i] = res_cg.wrapping_add(if i == 0 { 0 } else { cur_cg[i - 1] });
                }
                PredictMode::Up => {
                    cur_y[i] = res_y.wrapping_add(prev_y[i]);
                    cur_co[i] = res_co.wrapping_add(prev_co[i]);
                    cur_cg[i] = res_cg.wrapping_add(prev_cg[i]);
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
                    cur_y[i] = res_y.wrapping_add(py);
                    cur_co[i] = res_co.wrapping_add(pco);
                    cur_cg[i] = res_cg.wrapping_add(pcg);
                }
                PredictMode::Mean => {
                    if i == 0 {
                        cur_y[i] = res_y.wrapping_add(prev_y[i]);
                        cur_co[i] = res_co.wrapping_add(prev_co[i]);
                        cur_cg[i] = res_cg.wrapping_add(prev_cg[i]);
                    } else {
                        // The (left + up + 1) sum WRAPS at i16 before the
                        // truncation fix and shift — differentially pinned
                        // against vImageDeepmap2Decode on 16-bit streams
                        // whose sums cross ±32768 (an i32 sum here decoded
                        // those streams measurably differently; for 8-bit
                        // streams the sum can never leave i16, so this is
                        // behavior-neutral there).
                        let mean16 = |left: i16, up: i16| -> i16 {
                            let mut s = left.wrapping_add(up).wrapping_add(1);
                            if s < 0 { s += 1; }
                            s >> 1
                        };
                        cur_y[i] = res_y.wrapping_add(mean16(cur_y[i-1], prev_y[i]));
                        cur_co[i] = res_co.wrapping_add(mean16(cur_co[i-1], prev_co[i]));
                        cur_cg[i] = res_cg.wrapping_add(mean16(cur_cg[i-1], prev_cg[i]));
                    }
                }
            }
        }

        // Standard inverse YCoCg (no color un-decrement — the un-adjustment
        // above already recovers original Co/Cg values)
        let ps = format.pixel_size();
        let row_pixels = &mut pixels[row * w * ps..(row + 1) * w * ps];

        match format {
            PixelFormat::Rgba8 => {
                for i in 0..w {
                    let co = cur_co[i].wrapping_mul(chroma_scale);
                    let cg = cur_cg[i].wrapping_mul(chroma_scale);
                    let t = cur_y[i].wrapping_sub(cg / 2);
                    let g = cg.wrapping_add(t);
                    let b = t.wrapping_sub(co / 2);
                    let r = co.wrapping_add(b);
                    row_pixels[i * 4] = r.clamp(0, 255) as u8;
                    row_pixels[i * 4 + 1] = g.clamp(0, 255) as u8;
                    row_pixels[i * 4 + 2] = b.clamp(0, 255) as u8;
                    row_pixels[i * 4 + 3] = buf[row * w + i];
                }
            }
            PixelFormat::Rgb8 => {
                for i in 0..w {
                    let co = cur_co[i].wrapping_mul(chroma_scale);
                    let cg = cur_cg[i].wrapping_mul(chroma_scale);
                    let t = cur_y[i].wrapping_sub(cg / 2);
                    let g = cg.wrapping_add(t);
                    let b = t.wrapping_sub(co / 2);
                    let r = co.wrapping_add(b);
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
            PixelFormat::GrayA16 => {
                // Same K=3 layout as GrayA8 (8-bit alpha plane + Y hi/lo
                // planes); the Y codes are fixed-point half-float values,
                // alpha expands to half(a/255) — exactly the RGBA16 output
                // rule minus the chroma channels. Quality has no effect
                // (no chroma), same as GrayA8.
                let scale = (1u32 << (param - 1)) as f32;
                for i in 0..w {
                    let a = buf[row * w + i];
                    let px = &mut row_pixels[i * 4..i * 4 + 4];
                    px[0..2].copy_from_slice(&f32_to_f16_bits(cur_y[i] as f32 / scale).to_le_bytes());
                    px[2..4].copy_from_slice(&f32_to_f16_bits(a as f32 / 255.0).to_le_bytes());
                }
            }
            PixelFormat::Rgb16 => {
                // RGBA16's output rule without the alpha plane (K=6, same
                // layout as RGB8): wrapping inverse YCoCg on fixed-point
                // codes, half-scale chroma when quality != 0.
                let scale = (1u32 << (param - 1)) as f32;
                for i in 0..w {
                    let co = cur_co[i].wrapping_mul(chroma_scale);
                    let cg = cur_cg[i].wrapping_mul(chroma_scale);
                    let t = cur_y[i].wrapping_sub(cg / 2);
                    let g = cg.wrapping_add(t);
                    let b = t.wrapping_sub(co / 2);
                    let r = co.wrapping_add(b);
                    let px = &mut row_pixels[i * 6..i * 6 + 6];
                    px[0..2].copy_from_slice(&f32_to_f16_bits(r as f32 / scale).to_le_bytes());
                    px[2..4].copy_from_slice(&f32_to_f16_bits(g as f32 / scale).to_le_bytes());
                    px[4..6].copy_from_slice(&f32_to_f16_bits(b as f32 / scale).to_le_bytes());
                }
            }
            PixelFormat::Rgba16 => {
                // 16-bit type 2 uses the SAME intermediate layout as RGBA8
                // (1-byte alpha plane + 16-bit YCoCg zigzag residuals in
                // high/low byte planes, K=7); only the output stage differs.
                // The reconstructed integers are fixed-point codes of
                // half-float channel values: value = code / 2^(param-1)
                // (param 9..=12; real .car renditions use 10 -> /512), and
                // the 8-bit alpha plane expands to half(a/255). The output
                // pixels are IEEE half bit patterns, little-endian — byte-
                // identical to vImageDeepmap2Decode (verified on
                // Apple-encoded fixtures).
                let scale = (1u32 << (param - 1)) as f32;
                for i in 0..w {
                    // Inverse YCoCg with WRAPPING i16 arithmetic — like the
                    // prediction stage, this must wrap exactly where Apple's
                    // 16-bit lanes do (verified: codes pushed past ±32768 by
                    // the encoder come out sign-flipped from Apple's decoder
                    // too). `/ 2` truncates toward zero, matching Apple.
                    let co = cur_co[i].wrapping_mul(chroma_scale);
                    let cg = cur_cg[i].wrapping_mul(chroma_scale);
                    let t = cur_y[i].wrapping_sub(cg / 2);
                    let g = cg.wrapping_add(t);
                    let b = t.wrapping_sub(co / 2);
                    let r = co.wrapping_add(b);
                    let a = buf[row * w + i];
                    let px = &mut row_pixels[i * 8..i * 8 + 8];
                    px[0..2].copy_from_slice(&f32_to_f16_bits(r as f32 / scale).to_le_bytes());
                    px[2..4].copy_from_slice(&f32_to_f16_bits(g as f32 / scale).to_le_bytes());
                    px[4..6].copy_from_slice(&f32_to_f16_bits(b as f32 / scale).to_le_bytes());
                    px[6..8].copy_from_slice(&f32_to_f16_bits(a as f32 / 255.0).to_le_bytes());
                }
            }
            // 1-channel formats are routed to the gray decoder before
            // reaching here; reject explicitly rather than panic if a
            // future caller breaks the dispatch invariant.
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
