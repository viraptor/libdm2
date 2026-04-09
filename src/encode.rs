use crate::error::{Dm2Error, Result};
use crate::format::*;
use crate::lzfse;
use crate::predict::{self, PredictMode};

pub fn encode(pixels: &[u8], info: &ImageInfo, compression: Compression) -> Result<Vec<u8>> {
    if info.width == 0 || info.height == 0 {
        return Err(Dm2Error::InvalidArg);
    }
    let need = info.checked_raw_size()?;
    if pixels.len() < need {
        return Err(Dm2Error::BufferTooSmall);
    }
    match compression {
        Compression::None => encode_none(pixels, info),
        Compression::Lossless => encode_lossless(pixels, info),
        Compression::Default => encode_default(pixels, info),
        Compression::Palette => encode_palette(pixels, info),
    }
}

pub fn encode_auto(pixels: &[u8], info: &ImageInfo) -> Result<Vec<u8>> {
    // Start with None as fallback (always works, even for incompressible data)
    let mut best = encode(pixels, info, Compression::None)?;

    if let Ok(l) = encode(pixels, info, Compression::Lossless) {
        if l.len() < best.len() { best = l; }
    }

    if let Ok(d) = encode(pixels, info, Compression::Default) {
        if d.len() < best.len() {
            best = d;
        }
    }

    if info.format == PixelFormat::Rgba8 {
        if let Ok(p) = encode(pixels, info, Compression::Palette) {
            if p.len() < best.len() {
                best = p;
            }
        }
    }

    Ok(best)
}

fn write_header(info: &ImageInfo, compression: Compression, tile_h: u32, palette: Option<&[[u8; 4]]>) -> Result<Vec<u8>> {
    let hdr = Header {
        compression,
        quality: 0,
        param: 0,
        format: info.format,
        tile_width: info.width as u16,
        tile_height: tile_h as u16,
        palette: palette.map(|p| p.to_vec()),
        palette_bpe: 4,
    };
    let hdr_len = 12 + palette.map(|p| 4 + p.len() * 4).unwrap_or(0);
    let mut buf = vec![0u8; hdr_len];
    hdr.write(&mut buf)?;
    Ok(buf)
}

fn encode_none(pixels: &[u8], info: &ImageInfo) -> Result<Vec<u8>> {
    let mut out = write_header(info, Compression::None, info.height, None)?;
    let row_bytes = info.row_bytes();
    for y in 0..info.height as usize {
        out.extend_from_slice(&pixels[y * row_bytes..(y + 1) * row_bytes]);
    }
    Ok(out)
}

fn encode_lossless(pixels: &[u8], info: &ImageInfo) -> Result<Vec<u8>> {
    let tile_h = compute_tile_height(Compression::Lossless, info.width, info.height, info.format.pixel_size());
    let mut out = write_header(info, Compression::Lossless, tile_h, None)?;
    let row_bytes = info.row_bytes();

    let mut y = 0u32;
    while y < info.height {
        let rows = tile_h.min(info.height - y) as usize;
        let tile_start = y as usize * row_bytes;
        let tile_end = tile_start + rows * row_bytes;
        let tile_pixels = &pixels[tile_start..tile_end];

        let compressed = lzfse::compress(tile_pixels)?;
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&compressed);

        y += tile_h;
    }
    Ok(out)
}

/// Why our type-2 encoder cannot match Apple's output for a given image.
/// Each variant identifies an Apple-compatible code path we have not yet
/// reverse-engineered. When any of these applies the encoder transparently
/// falls back to lossless (type 3).
#[derive(Debug, Clone, Copy)]
enum DefaultUnsupported {
    /// Type 2 has no defined encoding for 16-bit pixel formats — Apple's
    /// own type-2 round-trip is broken for them (see deepmap2.md).
    SixteenBit,
    /// The intermediate buffer is too small for LZFSE to amortize its
    /// per-block overhead, or the image is too narrow/short for the
    /// prediction pipeline to be applied.
    ImageTooSmall,
}

/// Decide whether `info`/`pixels` can go through the Apple-compatible
/// type-2 pipeline. Returns `None` if it can; otherwise the specific
/// limitation that forces a lossless fallback.
fn default_limitation(_pixels: &[u8], info: &ImageInfo) -> Option<DefaultUnsupported> {
    if info.format.is_16bit() {
        return Some(DefaultUnsupported::SixteenBit);
    }
    let k = byte_planes_for(info.format);
    let int_size = info.height as usize * (k * info.width as usize + 1);
    if int_size < 4096 || info.width < 2 || info.height < 2 {
        return Some(DefaultUnsupported::ImageTooSmall);
    }
    None
}

/// Type 2 (Default) encoding with the Apple-compatible pipeline.
///
/// This is a strict subset of Apple's encoder. The cases we **don't**
/// implement are enumerated by [`DefaultUnsupported`] and trigger a
/// transparent fallback to lossless (type 3) so callers always get a
/// valid stream. The decoder, by contrast, supports the full type-2
/// format including all five prediction modes.
fn encode_default(pixels: &[u8], info: &ImageInfo) -> Result<Vec<u8>> {
    match default_limitation(pixels, info) {
        // 16-bit is a hard rejection: Apple's own type-2 round-trip is
        // broken for these formats, so we refuse rather than silently
        // pick a different scheme.
        Some(DefaultUnsupported::SixteenBit) => return Err(Dm2Error::BadFormat),
        // Every other limitation is a transparent fallback to lossless.
        Some(_) => return encode_lossless(pixels, info),
        None => {}
    }
    let tile_h = compute_tile_height(Compression::Default, info.width, info.height, info.format.pixel_size());
    let mut out = write_header(info, Compression::Default, tile_h, None)?;
    let row_bytes = info.row_bytes();
    let w = info.width as usize;

    let mut y = 0u32;
    while y < info.height {
        let rows = tile_h.min(info.height - y) as usize;
        let tile_pixels = &pixels[y as usize * row_bytes..(y as usize + rows) * row_bytes];
        let compressed = encode_default_tile(tile_pixels, w, rows, info.format)?;
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&compressed);
        y += tile_h;
    }
    Ok(out)
}

fn encode_default_tile(pixels: &[u8], w: usize, h: usize, format: PixelFormat) -> Result<Vec<u8>> {
    let channels = format.channels();
    if channels >= 2 {
        return encode_default_tile_ycc(pixels, w, h, format);
    }
    encode_default_tile_gray(pixels, w, h, format)
}

/// Gray (1-channel) type 2 tile encoding.
/// Layout: [H modes][H*W high_bytes][H*W low_bytes]
/// Mode byte = prediction mode (0=None, 2=Left, 3=Up).
fn encode_default_tile_gray(pixels: &[u8], w: usize, h: usize, format: PixelFormat) -> Result<Vec<u8>> {
    let k = byte_planes_for(format);
    let buf_size = (h * (k * w + 1) + 15) & !15;
    let mut buf = vec![0u8; buf_size];

    let mut cur = vec![0i16; w];
    let mut prev = vec![0i16; w];
    let mut residuals = vec![0i16; w];
    let mut scratch = vec![0i16; w];

    for row in 0..h {
        let row_pixels = &pixels[row * w..(row + 1) * w];
        for i in 0..w { cur[i] = row_pixels[i] as i16; }

        let prev_ref = if row > 0 { Some(prev.as_slice()) } else { None };
        let mode = predict::predict_row(&cur, prev_ref, &mut residuals, &mut scratch);
        buf[row] = mode as u8;

        for i in 0..w {
            // predict_row only selects None, Left, or Up for gray
            let pred = match mode {
                PredictMode::None => 0i16,
                PredictMode::Left => if i == 0 { 0 } else { cur[i - 1] },
                _ => prev[i],
            };
            let mut res = cur[i] - pred;
            if mode != PredictMode::None && res < 0 {
                res -= 1;
            }
            let z = predict::zigzag_encode(res);
            buf[h + row * w + i] = (z >> 8) as u8;
            buf[h + w * h + row * w + i] = z as u8;
        }

        std::mem::swap(&mut cur, &mut prev);
    }

    lzfse::compress(&buf)
}

/// RGB/RGBA (3-4 channel) type 2 tile encoding.
/// Layout: [H modes][H*W alpha][H*3*W high_interleaved][H*3*W low_interleaved]
/// The alpha plane's last row stores YCC prediction modes (all 0 = None).
/// Mode byte = first pixel alpha per row. YCC values stored as raw adjusted zigzag.
///
/// **Limitation:** this encoder always emits prediction mode 0 (None) for
/// every row. The decoder handles all five modes — Apple's encoder picks
/// the cheapest per row — but reproducing Apple's mode-selection heuristic
/// for multi-channel data is not yet implemented. Output is still valid
/// and round-trippable, just larger than Apple's for structured images.
fn encode_default_tile_ycc(pixels: &[u8], w: usize, h: usize, format: PixelFormat) -> Result<Vec<u8>> {
    let channels = format.channels();
    let has_alpha = channels == 2 || channels == 4;
    let k = byte_planes_for(format);
    let buf_size = (h * (k * w + 1) + 15) & !15;
    let mut buf = vec![0u8; buf_size];

    let n_color = if channels <= 2 { 1 } else { 3 };
    let alpha_plane_size = if has_alpha { w * h } else { 0 };
    let modes_off = alpha_plane_size;
    let high_off = alpha_plane_size + h;
    let low_off = high_off + n_color * w * h;

    for row in 0..h {
        let ps = format.pixel_size();
        let row_pixels = &pixels[row * w * ps..(row + 1) * w * ps];

        let mut cur_y = vec![0i16; w];
        let mut cur_co = vec![0i16; w];
        let mut cur_cg = vec![0i16; w];

        let alpha_ch = ps - 1;
        match format {
            PixelFormat::Rgba8 => {
                for i in 0..w {
                    let r = row_pixels[i * 4] as i16;
                    let g = row_pixels[i * 4 + 1] as i16;
                    let b = row_pixels[i * 4 + 2] as i16;
                    let co = r - b;
                    let t = b + co / 2;
                    let cg = g - t;
                    cur_y[i] = t + cg / 2;
                    cur_co[i] = co;
                    cur_cg[i] = cg;
                }
            }
            PixelFormat::Rgb8 => {
                for i in 0..w {
                    let r = row_pixels[i * 3] as i16;
                    let g = row_pixels[i * 3 + 1] as i16;
                    let b = row_pixels[i * 3 + 2] as i16;
                    let co = r - b;
                    let t = b + co / 2;
                    let cg = g - t;
                    cur_y[i] = t + cg / 2;
                    cur_co[i] = co;
                    cur_cg[i] = cg;
                }
            }
            PixelFormat::GrayA8 => {
                for i in 0..w { cur_y[i] = row_pixels[i * 2] as i16; }
            }
            // The dispatch in encode_default_tile already routed 1-channel
            // and 16-bit formats elsewhere; defend against future callers.
            _ => return Err(Dm2Error::BadFormat),
        }

        buf[modes_off + row] = 0; // mode 0 (None) for all rows

        if has_alpha {
            for i in 0..w {
                buf[row * w + i] = row_pixels[i * ps + alpha_ch];
            }
        }

        // Mode 0 (None): apply per-residual decrement and store
        for i in 0..w {
            let mut vy = cur_y[i]; if vy < 0 { vy -= 1; }
            let zz_y = predict::zigzag_encode(vy);
            let hi = high_off + row * n_color * w + i * n_color;
            let lo = low_off + row * n_color * w + i * n_color;
            buf[hi] = (zz_y >> 8) as u8;
            buf[lo] = zz_y as u8;
            if n_color >= 3 {
                let mut vco = cur_co[i]; if vco < 0 { vco -= 1; }
                let mut vcg = cur_cg[i]; if vcg < 0 { vcg -= 1; }
                let zz_co = predict::zigzag_encode(vco);
                let zz_cg = predict::zigzag_encode(vcg);
                buf[hi + 1] = (zz_co >> 8) as u8;
                buf[hi + 2] = (zz_cg >> 8) as u8;
                buf[lo + 1] = zz_co as u8;
                buf[lo + 2] = zz_cg as u8;
            }
        }
    }

    lzfse::compress(&buf)
}



/// Number of byte-planes per pixel position in the type-2 intermediate
/// buffer. Driven directly by the pixel format so the type system covers
/// every variant.
pub(crate) fn byte_planes_for(format: PixelFormat) -> usize {
    match format {
        PixelFormat::Gray8  | PixelFormat::Gray16  => 2,
        PixelFormat::GrayA8 | PixelFormat::GrayA16 => 3,
        PixelFormat::Rgb8   | PixelFormat::Rgb16   => 6,
        PixelFormat::Rgba8  | PixelFormat::Rgba16  => 7,
    }
}

fn encode_palette(pixels: &[u8], info: &ImageInfo) -> Result<Vec<u8>> {
    if info.format != PixelFormat::Rgba8 {
        return Err(Dm2Error::BadFormat);
    }

    let w = info.width as usize;
    let h = info.height as usize;
    let npix = w * h;

    // Build palette: collect unique RGBA values
    let mut colors: Vec<[u8; 4]> = Vec::new();
    let mut indices = vec![0u8; npix];

    use std::collections::HashMap;
    let mut color_map: HashMap<[u8; 4], u8> = HashMap::new();

    for i in 0..npix {
        let c = [pixels[i * 4], pixels[i * 4 + 1], pixels[i * 4 + 2], pixels[i * 4 + 3]];
        let idx = if let Some(&idx) = color_map.get(&c) {
            idx
        } else {
            // Bounds-check *before* inserting so we never grow `colors`
            // past the 256-entry limit even transiently.
            if colors.len() == 256 {
                return Err(Dm2Error::EncodeFailed);
            }
            let new_idx = colors.len() as u8;
            colors.push(c);
            color_map.insert(c, new_idx);
            new_idx
        };
        indices[i] = idx;
    }

    let tile_h = compute_tile_height(Compression::Palette, info.width, info.height, 1);
    let mut out = write_header(info, Compression::Palette, tile_h, Some(&colors))?;

    let mut y = 0u32;
    while y < info.height {
        let rows = tile_h.min(info.height - y) as usize;
        let tile_start = y as usize * w;
        let tile_end = tile_start + rows * w;
        let tile_indices = &indices[tile_start..tile_end];

        let compressed = lzfse::compress(tile_indices)?;
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&compressed);

        y += tile_h;
    }

    Ok(out)
}
