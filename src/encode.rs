use crate::color::f16_bits_to_f32;
use crate::error::{Dm2Error, Result};
use crate::format::*;
use crate::lzfse;

/// Encode with default options (quality 0; param 10 for 16-bit formats —
/// the fixed-point scale exponent real `.car` renditions use).
pub fn encode(pixels: &[u8], info: &ImageInfo, compression: Compression) -> Result<Vec<u8>> {
    let param = if info.format.is_16bit() { 10 } else { 0 };
    encode_opts(pixels, info, compression, 0, param)
}

/// Encode with explicit `quality`/`param`, mirroring Apple's
/// `Deepmap2Options`:
///
/// - `quality`: 0 or 1. For type 2, chroma (Co/Cg) is divided by
///   `1 << quality` — quality 1 halves it (the documented maxerr=1 loss on
///   R/B for RGB formats). No effect on gray/gray+alpha payloads. Apple
///   rejects quality >= 2; so do we.
/// - `param`: stored in the header. For 16-bit formats it selects the
///   type-2 fixed-point scale `2^(param-1)` and must be 9..=12 (Apple
///   rejects everything else for 16-bit); for 8-bit formats it has no
///   effect on the payload.
pub fn encode_opts(
    pixels: &[u8],
    info: &ImageInfo,
    compression: Compression,
    quality: u8,
    param: u8,
) -> Result<Vec<u8>> {
    if info.width == 0 || info.height == 0 {
        return Err(Dm2Error::InvalidArg);
    }
    if quality > 1 {
        return Err(Dm2Error::InvalidArg);
    }
    if info.format.is_16bit() && !(9..=12).contains(&param) {
        return Err(Dm2Error::InvalidArg);
    }
    let need = info.checked_raw_size()?;
    if pixels.len() < need {
        return Err(Dm2Error::BufferTooSmall);
    }
    match compression {
        Compression::None => encode_none(pixels, info, quality, param),
        Compression::Lossless => encode_lossless(pixels, info, quality, param),
        Compression::Default => encode_default(pixels, info, quality, param),
        Compression::Palette => encode_palette(pixels, info, quality, param),
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

fn write_header(
    info: &ImageInfo,
    compression: Compression,
    quality: u8,
    param: u8,
    tile_h: u32,
    palette: Option<&[[u8; 4]]>,
) -> Result<Vec<u8>> {
    let hdr = Header {
        compression,
        quality,
        param,
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

fn encode_none(pixels: &[u8], info: &ImageInfo, quality: u8, param: u8) -> Result<Vec<u8>> {
    let mut out = write_header(info, Compression::None, quality, param, info.height, None)?;
    let row_bytes = info.row_bytes();
    for y in 0..info.height as usize {
        out.extend_from_slice(&pixels[y * row_bytes..(y + 1) * row_bytes]);
    }
    Ok(out)
}

fn encode_lossless(pixels: &[u8], info: &ImageInfo, quality: u8, param: u8) -> Result<Vec<u8>> {
    let tile_h = compute_tile_height(Compression::Lossless, info.width, info.height, info.format);
    let mut out = write_header(info, Compression::Lossless, quality, param, tile_h, None)?;
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

/// Type 2 (Default) encoding — a byte-exact replica of Apple's encoder
/// pipeline (see deepmap2.md):
/// the intermediate buffer we compress is byte-identical to the one
/// Apple's encoder produces for the same input, so decoding our stream
/// yields exactly the pixels Apple's own encode→decode chain yields.
/// Only the LZFSE/LZVN compressed bytes differ (different match finder).
fn encode_default(pixels: &[u8], info: &ImageInfo, quality: u8, param: u8) -> Result<Vec<u8>> {
    // Apple's encoder rejects images with fewer than 4 pixels (1x1, 2x1,
    // 1x2). We keep the API promise of always returning a valid stream by
    // falling back to lossless for those.
    if (info.width as usize) * (info.height as usize) < 4 {
        return encode_lossless(pixels, info, quality, param);
    }
    let tile_h = compute_tile_height(Compression::Default, info.width, info.height, info.format);
    let mut out = write_header(info, Compression::Default, quality, param, tile_h, None)?;
    let row_bytes = info.row_bytes();
    let w = info.width as usize;

    let mut y = 0u32;
    while y < info.height {
        let rows = tile_h.min(info.height - y) as usize;
        let tile_pixels = &pixels[y as usize * row_bytes..(y as usize + rows) * row_bytes];
        let compressed = encode_default_tile(tile_pixels, w, rows, info.format, quality, param)?;
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&compressed);
        y += tile_h;
    }
    Ok(out)
}

// --- Apple-exact type-2 tile pipeline ---------------------------------
//
// Row values are 16-bit signed lanes, THREE per pixel: (Y, Co, Cg) for
// RGB formats, (Y, 0, 0) for gray formats (Apple's "Y00"). 8-bit
// channels load directly; 16-bit channels are half-floats quantized as
// wrap16(sat_i32(round_ties_away(f32(half) * 2^(param-1)))). The YCoCg
// transform runs in 16-bit lanes (wrapping at every step, truncating
// /2), and chroma is divided by `1 << quality` (sdiv, truncation).
//
// Mode selection per row evaluates None, Left, Up, Mean, Paeth IN THAT
// ORDER (row 0: None, Left), each winning only on strictly smaller cost.
// The cost is a bit-rate estimate: the f32 sum of logf(1 + |residual|)
// over all 3w lanes, accumulated sequentially (Paeth groups per pixel:
// s += (ly + lco) + lcg) — replicating Apple's accumulation order so
// near-tie decisions match. Residuals are computed in 32-bit on
// sign-extended i16 values and wrapped to i16 (Mean's left+up+1 sum and
// the Paeth selection do NOT wrap — encoder semantics differ from the
// decoder's i16-wrapping lanes for out-of-range values, faithfully
// reproducing Apple's encode/decode asymmetry on garbage inputs).

fn quant16(bits: u16, scale: f32) -> i16 {
    let v = f16_bits_to_f32(bits) * scale;
    // f32::round = ties away from zero (frinta); `as i32` saturates and
    // maps NaN to 0 (fcvtzs); `as i16` truncates (uzp1).
    (v.round() as i32) as i16
}

fn alpha16_to_u8(bits: u16) -> u8 {
    ((f16_bits_to_f32(bits) * 255.0).round() as i32).clamp(0, 255) as u8
}

/// Cost of one residual lane: logf(1 + |r|), abs and +1 in f64, log in
/// f32 — exactly the scvtf/fabs/fadd/fcvt/logf sequence in the kernels.
#[inline]
fn lane_cost(r: i16) -> f32 {
    ((1.0f64 + (r as f64).abs()) as f32).ln()
}

fn kernel_none(cur: &[i16], out: &mut [i16]) -> f32 {
    let mut s = 0.0f32;
    for (i, &v) in cur.iter().enumerate() {
        out[i] = v;
        s += lane_cost(v);
    }
    s
}

fn kernel_left(cur: &[i16], out: &mut [i16]) -> f32 {
    let mut s = 0.0f32;
    for i in 0..cur.len() {
        let r = if i < 3 { cur[i] } else { cur[i].wrapping_sub(cur[i - 3]) };
        out[i] = r;
        s += lane_cost(r);
    }
    s
}

fn kernel_up(cur: &[i16], up: &[i16], out: &mut [i16]) -> f32 {
    let mut s = 0.0f32;
    for i in 0..cur.len() {
        let r = cur[i].wrapping_sub(up[i]);
        out[i] = r;
        s += lane_cost(r);
    }
    s
}

fn kernel_mean(cur: &[i16], up: &[i16], out: &mut [i16]) -> f32 {
    let mut s = 0.0f32;
    for i in 0..cur.len() {
        let r = if i < 3 {
            cur[i].wrapping_sub(up[i])
        } else {
            // 32-bit sum + sign fix + arithmetic shift (no i16 wrap —
            // this is the ENCODER's mean; the decoder's wraps).
            let sum = cur[i - 3] as i32 + up[i] as i32 + 1;
            let sum = sum + ((sum as u32) >> 31) as i32;
            (cur[i] as i32).wrapping_sub(sum >> 1) as i16
        };
        out[i] = r;
        s += lane_cost(r);
    }
    s
}

fn kernel_paeth(cur: &[i16], up: &[i16], out: &mut [i16]) -> f32 {
    let w = cur.len() / 3;
    let mut s = 0.0f32;
    for l in 0..3 {
        let r = cur[l].wrapping_sub(up[l]);
        out[l] = r;
        s += lane_cost(r);
    }
    for px in 1..w {
        let i = px * 3;
        // 2-way Paeth select on the Y lane, applied to all three lanes.
        let left_y = cur[i - 3] as i32;
        let up_y = up[i] as i32;
        let p = up_y + left_y - up[i - 3] as i32;
        let sel_up = (p - left_y).abs() > (p - up_y).abs();
        let mut pix = 0.0f32;
        for l in 0..3 {
            let pred = if sel_up { up[i + l] } else { cur[i - 3 + l] };
            let r = cur[i + l].wrapping_sub(pred);
            out[i + l] = r;
            pix = if l == 0 { lane_cost(r) } else { pix + lane_cost(r) };
        }
        s += pix;
    }
    s
}

/// Adjust + zigzag one residual the way Apple stores it: negatives are
/// decremented in a wide register (-32768 becomes -32769 — no i16 wrap),
/// zigzag runs in 32-bit, and the low 16 bits are stored.
#[inline]
fn store_z16(r16: i16) -> u16 {
    let mut r = r16 as i32;
    if r < 0 { r -= 1; }
    let z = if r >= 0 { (r as u32) << 1 } else { (((-r) as u32) << 1) - 1 };
    z as u16
}

/// Build the 3-lane i16 value rows (+ 8-bit alpha plane) for one tile.
fn build_lanes(
    pixels: &[u8],
    w: usize,
    h: usize,
    format: PixelFormat,
    quality: u8,
    param: u8,
) -> (Vec<i16>, Vec<u8>) {
    let n = w * h;
    let mut lanes = vec![0i16; n * 3];
    let has_alpha = matches!(format, PixelFormat::GrayA8 | PixelFormat::GrayA16 | PixelFormat::Rgba8 | PixelFormat::Rgba16);
    let mut alpha = if has_alpha { vec![0u8; n] } else { vec![] };
    let ps = format.pixel_size();
    let scale = if format.is_16bit() { (1u32 << (param - 1)) as f32 } else { 1.0 };
    let chroma_div = 1i32 << quality;
    let le16 = |p: &[u8], off: usize| u16::from_le_bytes([p[off], p[off + 1]]);
    for i in 0..n {
        let px = &pixels[i * ps..(i + 1) * ps];
        let (y, co, cg) = match format {
            PixelFormat::Gray8 => (px[0] as i16, 0, 0),
            PixelFormat::GrayA8 => {
                alpha[i] = px[1];
                (px[0] as i16, 0, 0)
            }
            PixelFormat::Gray16 => (quant16(le16(px, 0), scale), 0, 0),
            PixelFormat::GrayA16 => {
                alpha[i] = alpha16_to_u8(le16(px, 2));
                (quant16(le16(px, 0), scale), 0, 0)
            }
            PixelFormat::Rgb8 | PixelFormat::Rgba8 | PixelFormat::Rgb16 | PixelFormat::Rgba16 => {
                let (r, g, b) = if format.is_16bit() {
                    if format == PixelFormat::Rgba16 {
                        alpha[i] = alpha16_to_u8(le16(px, 6));
                    }
                    (quant16(le16(px, 0), scale), quant16(le16(px, 2), scale), quant16(le16(px, 4), scale))
                } else {
                    if format == PixelFormat::Rgba8 {
                        alpha[i] = px[3];
                    }
                    (px[0] as i16, px[1] as i16, px[2] as i16)
                };
                // i16-lane YCoCg, truncating /2, wrapping at every step.
                let co = r.wrapping_sub(b);
                let t = b.wrapping_add((co as i32 / 2) as i16);
                let cg = g.wrapping_sub(t);
                let y = t.wrapping_add((cg as i32 / 2) as i16);
                (y, (co as i32 / chroma_div) as i16, (cg as i32 / chroma_div) as i16)
            }
        };
        lanes[i * 3] = y;
        lanes[i * 3 + 1] = co;
        lanes[i * 3 + 2] = cg;
    }
    (lanes, alpha)
}

fn encode_default_tile(
    pixels: &[u8],
    w: usize,
    h: usize,
    format: PixelFormat,
    quality: u8,
    param: u8,
) -> Result<Vec<u8>> {
    let (lanes, alpha) = build_lanes(pixels, w, h, format, quality, param);
    let n = w * h;
    let k = byte_planes_for(format);
    let n_color = match format {
        PixelFormat::Gray8 | PixelFormat::Gray16 | PixelFormat::GrayA8 | PixelFormat::GrayA16 => 1,
        _ => 3,
    };
    let gray_layout = matches!(format, PixelFormat::Gray8 | PixelFormat::Gray16);
    let buf_size = (h * (k * w + 1) + 15) & !15;
    let mut buf = vec![0u8; buf_size];

    let apane = if alpha.is_empty() { 0 } else { n };
    let modes_off = if gray_layout { 0 } else { apane };
    let hi_off = modes_off + h;
    let lo_off = hi_off + n_color * n;
    buf[..alpha.len()].copy_from_slice(&alpha);

    let mut best = vec![0i16; w * 3];
    let mut cand = vec![0i16; w * 3];
    for row in 0..h {
        let cur = &lanes[row * w * 3..(row + 1) * w * 3];
        let mut best_cost = kernel_none(cur, &mut best);
        let mut best_mode = 0u8;
        let consider = |mode: u8, cost: f32, res: &mut Vec<i16>, best_mode: &mut u8, best_cost: &mut f32, best: &mut Vec<i16>| {
            if cost < *best_cost {
                *best_mode = mode;
                *best_cost = cost;
                std::mem::swap(best, res);
            }
        };
        if row == 0 {
            let c = kernel_left(cur, &mut cand);
            consider(2, c, &mut cand, &mut best_mode, &mut best_cost, &mut best);
        } else {
            let up = &lanes[(row - 1) * w * 3..row * w * 3];
            let c = kernel_left(cur, &mut cand);
            consider(2, c, &mut cand, &mut best_mode, &mut best_cost, &mut best);
            let c = kernel_up(cur, up, &mut cand);
            consider(3, c, &mut cand, &mut best_mode, &mut best_cost, &mut best);
            let c = kernel_mean(cur, up, &mut cand);
            consider(4, c, &mut cand, &mut best_mode, &mut best_cost, &mut best);
            let c = kernel_paeth(cur, up, &mut cand);
            consider(1, c, &mut cand, &mut best_mode, &mut best_cost, &mut best);
        }
        buf[modes_off + row] = best_mode;
        for col in 0..w {
            for c in 0..n_color {
                let z = store_z16(best[col * 3 + c]);
                buf[hi_off + row * n_color * w + col * n_color + c] = (z >> 8) as u8;
                buf[lo_off + row * n_color * w + col * n_color + c] = z as u8;
            }
        }
    }

    lzfse::compress(&buf)
}

fn encode_palette(pixels: &[u8], info: &ImageInfo, quality: u8, param: u8) -> Result<Vec<u8>> {
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

    let tile_h = compute_tile_height(Compression::Palette, info.width, info.height, info.format);
    let mut out = write_header(info, Compression::Palette, quality, param, tile_h, Some(&colors))?;

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
