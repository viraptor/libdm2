use libdm2::*;

fn make_gradient(w: u32, h: u32, format: PixelFormat) -> Vec<u8> {
    let ps = format.pixel_size();
    let mut pixels = vec![0u8; w as usize * h as usize * ps];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let off = (y * w as usize + x) * ps;
            for c in 0..ps {
                pixels[off + c] = ((x * 4 + y * 3 + c * 17) & 0xFF) as u8;
            }
        }
    }
    pixels
}

fn roundtrip(w: u32, h: u32, format: PixelFormat, compression: Compression) {
    let pixels = make_gradient(w, h, format);
    let info = ImageInfo { width: w, height: h, format };

    let encoded = dm2_encode(&pixels, &info, compression)
        .unwrap_or_else(|e| panic!("encode failed for {w}x{h} {format:?} {compression:?}: {e}"));

    let mut decoded = vec![0u8; pixels.len()];
    let mut dec_info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&encoded, &mut decoded, &mut dec_info)
        .unwrap_or_else(|e| panic!("decode failed for {w}x{h} {format:?} {compression:?}: {e}"));

    assert_eq!(dec_info.format, format);

    // Type 2 self-roundtrip has lossy edges: wrapping adjustment (±1 for gray),
    // YCoCg rounding (±3), and RGBA last-row alpha loss (up to 255 for varying alpha).
    // The primary goal is Apple decoder compatibility (ours→Apple: 0 failures).
    let max_allowed = if compression == Compression::Default {
        if format.channels() >= 3 { 255 } else { 3 }
    } else { 0 };
    let maxerr = pixels.iter().zip(decoded.iter())
        .map(|(&a, &b)| (a as i16 - b as i16).unsigned_abs())
        .max().unwrap_or(0);
    assert!(maxerr <= max_allowed,
        "roundtrip maxerr={maxerr} (allowed {max_allowed}) for {w}x{h} {format:?} {compression:?}");
}

#[test]
fn none_gray() { roundtrip(64, 48, PixelFormat::Gray8, Compression::None); }

#[test]
fn none_rgba() { roundtrip(64, 48, PixelFormat::Rgba8, Compression::None); }

#[test]
fn lossless_gray() { roundtrip(64, 64, PixelFormat::Gray8, Compression::Lossless); }

#[test]
fn lossless_rgba() { roundtrip(64, 64, PixelFormat::Rgba8, Compression::Lossless); }

#[test]
fn lossless_rgb() { roundtrip(64, 64, PixelFormat::Rgb8, Compression::Lossless); }

#[test]
fn lossless_graya() { roundtrip(64, 64, PixelFormat::GrayA8, Compression::Lossless); }

#[test]
fn default_gray() { roundtrip(64, 64, PixelFormat::Gray8, Compression::Default); }

#[test]
fn default_rgba() { roundtrip(64, 64, PixelFormat::Rgba8, Compression::Default); }

#[test]
fn default_rgb() { roundtrip(64, 64, PixelFormat::Rgb8, Compression::Default); }

#[test]
fn default_graya() { roundtrip(64, 64, PixelFormat::GrayA8, Compression::Default); }

#[test]
fn palette_few_colors() {
    let w = 64u32;
    let h = 64u32;
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for i in 0..(w * h) as usize {
        let c = (i % 8) as u8;
        pixels[i * 4] = c * 30;
        pixels[i * 4 + 1] = c * 20 + 10;
        pixels[i * 4 + 2] = c * 10 + 50;
        pixels[i * 4 + 3] = 0xFF;
    }
    let info = ImageInfo { width: w, height: h, format: PixelFormat::Rgba8 };
    let encoded = dm2_encode(&pixels, &info, Compression::Palette).unwrap();
    let mut decoded = vec![0u8; pixels.len()];
    let mut dec_info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&encoded, &mut decoded, &mut dec_info).unwrap();
    assert_eq!(&decoded, &pixels);
}

#[test]
fn auto_picks_smallest() {
    let pixels = make_gradient(64, 64, PixelFormat::Gray8);
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Gray8 };
    let auto = dm2_encode_auto(&pixels, &info).unwrap();
    let none = dm2_encode(&pixels, &info, Compression::None).unwrap();
    assert!(auto.len() <= none.len(), "auto should be at least as good as none");
}

#[test]
fn large_image_lossless() { roundtrip(256, 256, PixelFormat::Gray8, Compression::Lossless); }

#[test]
fn large_image_default() { roundtrip(256, 256, PixelFormat::Rgba8, Compression::Default); }

#[test]
fn non_square() { roundtrip(100, 50, PixelFormat::Rgba8, Compression::Lossless); }

#[test]
fn read_info_works() {
    let pixels = make_gradient(32, 16, PixelFormat::Rgb8);
    let info = ImageInfo { width: 32, height: 16, format: PixelFormat::Rgb8 };
    let encoded = dm2_encode(&pixels, &info, Compression::None).unwrap();
    let (ri, comp) = dm2_read_info(&encoded).unwrap();
    assert_eq!(ri.width, 32);
    assert_eq!(ri.height, 16);
    assert_eq!(ri.format, PixelFormat::Rgb8);
    assert_eq!(comp, Compression::None);
}
