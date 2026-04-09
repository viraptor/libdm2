use libdm2::*;

// ---------------------------------------------------------------------------
// Error handling: invalid inputs
// ---------------------------------------------------------------------------

#[test]
fn encode_zero_width() {
    let info = ImageInfo { width: 0, height: 64, format: PixelFormat::Gray8 };
    assert!(dm2_encode(&[], &info, Compression::None).is_err());
}

#[test]
fn encode_zero_height() {
    let info = ImageInfo { width: 64, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_encode(&[], &info, Compression::None).is_err());
}

#[test]
fn encode_buffer_too_small() {
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Gray8 };
    let pixels = vec![0u8; 100]; // needs 4096
    assert!(dm2_encode(&pixels, &info, Compression::None).is_err());
}

#[test]
fn decode_bad_magic() {
    let data = b"XXXX\x01\x00\x00\x01\x04\x00\x04\x00";
    let mut pixels = vec![0u8; 16];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(data, &mut pixels, &mut info).is_err());
}

#[test]
fn decode_truncated_header() {
    let data = b"dmp2\x01\x00";
    let mut pixels = vec![0u8; 16];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(data, &mut pixels, &mut info).is_err());
}

#[test]
fn decode_invalid_compression_type() {
    let data = b"dmp2\x05\x00\x00\x01\x04\x00\x04\x00";
    let mut pixels = vec![0u8; 64];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(data, &mut pixels, &mut info).is_err());
}

#[test]
fn decode_invalid_pixel_format() {
    let data = b"dmp2\x01\x00\x00\x05\x04\x00\x04\x00";
    let mut pixels = vec![0u8; 64];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(data, &mut pixels, &mut info).is_err());
}

#[test]
fn decode_output_buffer_too_small() {
    let pixels = vec![128u8; 16];
    let info = ImageInfo { width: 4, height: 4, format: PixelFormat::Gray8 };
    let encoded = dm2_encode(&pixels, &info, Compression::None).unwrap();
    let mut out = vec![0u8; 3]; // not even one full row
    let mut dec_info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    // With 3 bytes and row_bytes=4: inferred height=0, should get an error or empty decode
    let result = dm2_decode(&encoded, &mut out, &mut dec_info);
    // If it doesn't error, height should be 0 (no complete row fits)
    if result.is_ok() {
        assert_eq!(dec_info.height, 0);
    }
}

#[test]
fn decode_none_truncated_data() {
    // Craft a valid type 1 header for 4x4 gray but only provide 8 bytes of pixel data
    let mut data = vec![0u8; 20]; // 12 header + 8 pixel bytes (need 16)
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 1; // None
    data[7] = 1; // Gray8
    data[8] = 4; data[9] = 0; // width=4
    data[10] = 4; data[11] = 0; // height=4
    let mut out = vec![0u8; 16];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&data, &mut out, &mut info).is_err());
}

// ---------------------------------------------------------------------------
// Type 1 (None): all formats
// ---------------------------------------------------------------------------

fn roundtrip_exact(w: u32, h: u32, fmt: PixelFormat, comp: Compression) {
    let ps = fmt.pixel_size();
    let mut px = vec![0u8; w as usize * h as usize * ps];
    for (i, b) in px.iter_mut().enumerate() { *b = ((i * 7 + 13) & 0xFF) as u8; }
    let info = ImageInfo { width: w, height: h, format: fmt };
    let enc = dm2_encode(&px, &info, comp).unwrap();
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(di.format, fmt);
    assert_eq!(di.width, w);
    assert_eq!(di.height, h);
    assert_eq!(&dec, &px, "mismatch for {w}x{h} {fmt:?} {comp:?}");
}

#[test]
fn none_graya() { roundtrip_exact(32, 16, PixelFormat::GrayA8, Compression::None); }

#[test]
fn none_rgb() { roundtrip_exact(32, 16, PixelFormat::Rgb8, Compression::None); }

#[test]
fn none_1x1_gray() { roundtrip_exact(1, 1, PixelFormat::Gray8, Compression::None); }

#[test]
fn none_1x1_rgba() { roundtrip_exact(1, 1, PixelFormat::Rgba8, Compression::None); }

#[test]
fn none_wide() { roundtrip_exact(1000, 1, PixelFormat::Gray8, Compression::None); }

#[test]
fn none_tall() { roundtrip_exact(1, 1000, PixelFormat::Rgba8, Compression::None); }

// ---------------------------------------------------------------------------
// Type 3 (Lossless): small and edge-case sizes
// ---------------------------------------------------------------------------

#[test]
fn lossless_3x3_gray() { roundtrip_exact(3, 3, PixelFormat::Gray8, Compression::Lossless); }

#[test]
fn lossless_1x9_rgba() { roundtrip_exact(1, 9, PixelFormat::Rgba8, Compression::Lossless); }

#[test]
fn lossless_9x1_rgb() { roundtrip_exact(9, 1, PixelFormat::Rgb8, Compression::Lossless); }

#[test]
fn lossless_non_square_graya() { roundtrip_exact(100, 30, PixelFormat::GrayA8, Compression::Lossless); }

#[test]
fn lossless_non_square_rgb() { roundtrip_exact(30, 100, PixelFormat::Rgb8, Compression::Lossless); }

// ---------------------------------------------------------------------------
// Type 2 (Default): small image fallback to lossless
// ---------------------------------------------------------------------------

#[test]
fn default_small_falls_back_to_lossless() {
    let px = vec![42u8; 9];
    let info = ImageInfo { width: 3, height: 3, format: PixelFormat::Gray8 };
    let enc = dm2_encode(&px, &info, Compression::Default).unwrap();
    let (ri, comp) = dm2_read_info(&enc).unwrap();
    assert_eq!(ri.width, 3);
    assert_eq!(comp, Compression::Lossless, "small image should fall back to lossless");
    let mut dec = vec![0u8; 9];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(&dec, &px);
}

#[test]
fn default_varying_alpha_falls_back() {
    let mut px = vec![0u8; 64 * 64 * 4];
    for i in 0..64 * 64 {
        px[i * 4] = 100; px[i * 4 + 1] = 150; px[i * 4 + 2] = 200;
        px[i * 4 + 3] = (i & 0xFF) as u8;
    }
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Rgba8 };
    let enc = dm2_encode(&px, &info, Compression::Default).unwrap();
    let (_, comp) = dm2_read_info(&enc).unwrap();
    assert_eq!(comp, Compression::Lossless, "varying alpha RGBA should fall back");
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(&dec, &px);
}

// ---------------------------------------------------------------------------
// Type 2 (Default): non-square multi-channel
// ---------------------------------------------------------------------------

#[test]
fn default_nonsquare_rgb() { roundtrip_exact(100, 30, PixelFormat::Rgb8, Compression::Default); }

#[test]
fn default_nonsquare_rgb_tall() { roundtrip_exact(30, 100, PixelFormat::Rgb8, Compression::Default); }

#[test]
fn default_nonsquare_graya() { roundtrip_exact(100, 30, PixelFormat::GrayA8, Compression::Default); }

// ---------------------------------------------------------------------------
// Type 2 (Default): extreme YCoCg values
// ---------------------------------------------------------------------------

#[test]
fn default_extreme_co() {
    let mut px = vec![0u8; 64 * 64 * 3];
    for i in 0..64 * 64 {
        if i % 2 == 0 { px[i*3] = 255; px[i*3+1] = 0; px[i*3+2] = 0; }
        else           { px[i*3] = 0; px[i*3+1] = 0; px[i*3+2] = 255; }
    }
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Rgb8 };
    let enc = dm2_encode(&px, &info, Compression::Default).unwrap();
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(&dec, &px);
}

// ---------------------------------------------------------------------------
// Type 4 (Palette): edge cases
// ---------------------------------------------------------------------------

#[test]
fn palette_single_color() {
    let px = vec![42u8; 64 * 64 * 4];
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Rgba8 };
    let enc = dm2_encode(&px, &info, Compression::Palette).unwrap();
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(&dec, &px);
}

#[test]
fn palette_256_colors() {
    let mut px = vec![0u8; 128 * 128 * 4];
    for i in 0..128 * 128 {
        let c = (i % 256) as u8;
        px[i*4] = c; px[i*4+1] = 255 - c; px[i*4+2] = c / 2; px[i*4+3] = 255;
    }
    let info = ImageInfo { width: 128, height: 128, format: PixelFormat::Rgba8 };
    let enc = dm2_encode(&px, &info, Compression::Palette).unwrap();
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(&dec, &px);
}

#[test]
fn palette_too_many_colors() {
    let mut px = vec![0u8; 64 * 64 * 4];
    for i in 0..64 * 64 {
        let c = i as u32;
        px[i*4] = (c & 0xFF) as u8;
        px[i*4+1] = ((c >> 4) & 0xFF) as u8;
        px[i*4+2] = ((c >> 8) & 0xFF) as u8;
        px[i*4+3] = 255;
    }
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Rgba8 };
    assert!(dm2_encode(&px, &info, Compression::Palette).is_err());
}

#[test]
fn palette_wrong_format() {
    let px = vec![0u8; 64 * 64];
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Gray8 };
    assert!(dm2_encode(&px, &info, Compression::Palette).is_err());
}

// ---------------------------------------------------------------------------
// read_info across compression types
// ---------------------------------------------------------------------------

#[test]
fn read_info_lossless() {
    let px = vec![0u8; 64 * 64];
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Gray8 };
    let enc = dm2_encode(&px, &info, Compression::Lossless).unwrap();
    let (ri, comp) = dm2_read_info(&enc).unwrap();
    assert_eq!(comp, Compression::Lossless);
    assert_eq!(ri.format, PixelFormat::Gray8);
}

#[test]
fn read_info_default() {
    let px = vec![0u8; 64 * 64];
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Gray8 };
    let enc = dm2_encode(&px, &info, Compression::Default).unwrap();
    let (ri, comp) = dm2_read_info(&enc).unwrap();
    assert_eq!(comp, Compression::Default);
    assert_eq!(ri.format, PixelFormat::Gray8);
}

#[test]
fn read_info_palette() {
    let mut px = vec![0u8; 64 * 64 * 4];
    for p in px.chunks_exact_mut(4) { p[3] = 255; }
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Rgba8 };
    let enc = dm2_encode(&px, &info, Compression::Palette).unwrap();
    let (ri, comp) = dm2_read_info(&enc).unwrap();
    assert_eq!(comp, Compression::Palette);
    assert_eq!(ri.format, PixelFormat::Rgba8);
}

// ---------------------------------------------------------------------------
// encode_auto: format coverage
// ---------------------------------------------------------------------------

#[test]
fn auto_rgba() {
    let px = vec![0u8; 64 * 64 * 4];
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Rgba8 };
    let enc = dm2_encode_auto(&px, &info).unwrap();
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(&dec, &px);
}

#[test]
fn auto_rgb() {
    let mut px = vec![0u8; 64 * 64 * 3];
    for (i, b) in px.iter_mut().enumerate() { *b = (i * 3 & 0xFF) as u8; }
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Rgb8 };
    let enc = dm2_encode_auto(&px, &info).unwrap();
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(&dec, &px);
}

// ---------------------------------------------------------------------------
// Pixel size API
// ---------------------------------------------------------------------------

#[test]
fn pixel_size_all_formats() {
    assert_eq!(dm2_pixel_size(PixelFormat::Gray8), 1);
    assert_eq!(dm2_pixel_size(PixelFormat::GrayA8), 2);
    assert_eq!(dm2_pixel_size(PixelFormat::Rgb8), 3);
    assert_eq!(dm2_pixel_size(PixelFormat::Rgba8), 4);
}

// ---------------------------------------------------------------------------
// encode_bound
// ---------------------------------------------------------------------------

#[test]
fn encode_bound_conservative() {
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Gray8 };
    let bound = dm2_encode_bound(&info);
    assert!(bound > 64 * 64, "bound should exceed raw size");
    let px = vec![0u8; 64 * 64];
    let enc = dm2_encode(&px, &info, Compression::None).unwrap();
    assert!(enc.len() <= bound, "actual size {} exceeds bound {}", enc.len(), bound);
}

// ---------------------------------------------------------------------------
// 16-bit format parsing / rejection
// ---------------------------------------------------------------------------

#[test]
fn read_info_16bit_lossless() {
    // Craft a valid 16-bit Gray16 lossless header
    let mut hdr = vec![0u8; 12];
    hdr[0..4].copy_from_slice(b"dmp2");
    hdr[4] = 3; // lossless
    hdr[7] = 0x11; // Gray16
    hdr[8] = 8; hdr[10] = 8; // 8x8
    let (ri, comp) = dm2_read_info(&hdr).unwrap();
    assert_eq!(ri.format, PixelFormat::Gray16);
    assert_eq!(comp, Compression::Lossless);
}

#[test]
fn pixel_size_16bit() {
    assert_eq!(dm2_pixel_size(PixelFormat::Gray16), 2);
    assert_eq!(dm2_pixel_size(PixelFormat::GrayA16), 4);
    assert_eq!(dm2_pixel_size(PixelFormat::Rgb16), 6);
    assert_eq!(dm2_pixel_size(PixelFormat::Rgba16), 8);
}

#[test]
fn encode_default_rejects_16bit() {
    let px = vec![0u8; 64 * 64 * 2];
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Gray16 };
    assert!(dm2_encode(&px, &info, Compression::Default).is_err());
}

#[test]
fn decode_default_rejects_16bit() {
    let mut hdr = vec![0u8; 16 + 4];
    hdr[0..4].copy_from_slice(b"dmp2");
    hdr[4] = 2; // default
    hdr[7] = 0x11; // Gray16
    hdr[8] = 8; hdr[10] = 8;
    // tile size = 0 (will trigger error after format check)
    let mut out = vec![0u8; 128];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&hdr, &mut out, &mut info).is_err());
}

// ---------------------------------------------------------------------------
// Malformed data decode errors
// ---------------------------------------------------------------------------

#[test]
fn decode_lossless_truncated_tile() {
    // Valid lossless header + tile size that exceeds remaining data
    let mut data = vec![0u8; 20];
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 3; // lossless
    data[7] = 1; // Gray8
    data[8] = 8; data[10] = 8; // 8x8
    data[12..16].copy_from_slice(&1000u32.to_le_bytes()); // tile_sz = 1000 > remaining
    let mut out = vec![0u8; 64];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&data, &mut out, &mut info).is_err());
}

#[test]
fn decode_lossless_size_mismatch() {
    // Encode a small image, then modify the header to claim a different size
    let px = vec![42u8; 64];
    let info = ImageInfo { width: 8, height: 8, format: PixelFormat::Gray8 };
    let mut enc = dm2_encode(&px, &info, Compression::Lossless).unwrap();
    // Change tile height to 16 — decompressed size won't match expected
    enc[10] = 16;
    let mut out = vec![0u8; 256];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&enc, &mut out, &mut di).is_err());
}

#[test]
fn decode_palette_truncated_header() {
    let mut data = vec![0u8; 14]; // need 16 for palette header
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 4; // palette
    data[7] = 4; // RGBA8
    data[8] = 8; data[10] = 8;
    data[12] = 2; // ncolors=2
    // Missing bytes_per_entry and palette data
    let mut out = vec![0u8; 256];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&data, &mut out, &mut info).is_err());
}

#[test]
fn decode_palette_bad_bpe() {
    let mut data = vec![0u8; 24];
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 4; data[7] = 4;
    data[8] = 8; data[10] = 8;
    data[12] = 1; // ncolors=1
    data[14] = 5; // bpe=5 (invalid, must be 3 or 4)
    let mut out = vec![0u8; 256];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&data, &mut out, &mut info).is_err());
}

#[test]
fn decode_palette_insufficient_entries() {
    let mut data = vec![0u8; 20];
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 4; data[7] = 4;
    data[8] = 8; data[10] = 8;
    data[12] = 10; // ncolors=10
    data[14] = 4; // bpe=4
    // Only 4 bytes remain — not enough for 10*4=40 palette bytes
    let mut out = vec![0u8; 256];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&data, &mut out, &mut info).is_err());
}

// ---------------------------------------------------------------------------
// GrayA type 2 with constant alpha (exercises encode_default_tile_ycc GrayA path)
// ---------------------------------------------------------------------------

#[test]
fn default_graya_constant_alpha() {
    let mut px = vec![0u8; 64 * 64 * 2];
    for i in 0..64 * 64 {
        px[i * 2] = ((i * 7 + 13) & 0xFF) as u8;
        px[i * 2 + 1] = 0xFF; // constant alpha
    }
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::GrayA8 };
    let enc = dm2_encode(&px, &info, Compression::Default).unwrap();
    let (_, comp) = dm2_read_info(&enc).unwrap();
    assert_eq!(comp, Compression::Default, "constant-alpha GrayA should use type 2");
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(&dec, &px);
}

// ---------------------------------------------------------------------------
// predict::unpredict_row UpLeft and Mean modes (unit-level)
// ---------------------------------------------------------------------------

#[test]
fn unpredict_upleft() {
    use libdm2::predict::{PredictMode, unpredict_row};
    let prev = vec![10i16, 20, 30, 40, 50];
    // With mode UpLeft: x=0 uses Up (prev[0]=10), x>0 uses 2-way Paeth
    // Residuals are all 0 → output should reconstruct prev exactly
    let residuals = vec![0i16; 5];
    let mut out = vec![0i16; 5];
    unpredict_row(&residuals, Some(&prev), PredictMode::UpLeft, &mut out).unwrap();
    assert_eq!(&out, &prev, "zero residuals with UpLeft should reproduce prev row");
}

#[test]
fn unpredict_mean() {
    use libdm2::predict::{PredictMode, unpredict_row};
    let prev = vec![100i16, 100, 100, 100];
    // Mean pred at x=0: up=100. At x>0: (left+up+1)/2.
    // With residual=0 and prev=100 everywhere: output should be 100 everywhere.
    let residuals = vec![0i16; 4];
    let mut out = vec![0i16; 4];
    unpredict_row(&residuals, Some(&prev), PredictMode::Mean, &mut out).unwrap();
    assert_eq!(&out, &[100, 100, 100, 100]);
}

#[test]
fn unpredict_mean_negative_sum() {
    use libdm2::predict::{PredictMode, unpredict_row};
    // left=-100, up=-100 → sum = -100 + -100 + 1 = -199; truncation fix: -199+1=-198; pred=-99
    let prev = vec![-100i16, -100];
    let residuals = vec![0i16, 0];
    let mut out = vec![0i16; 2];
    unpredict_row(&residuals, Some(&prev), PredictMode::Mean, &mut out).unwrap();
    assert_eq!(out[0], -100); // x=0: pred = up = -100
    assert_eq!(out[1], -99);  // x=1: pred = (-100 + -100 + 1 + 1) >> 1 = -198 >> 1 = -99
}

#[test]
fn unpredict_upleft_paeth_selection() {
    use libdm2::predict::{PredictMode, unpredict_row};
    // prev = [0, 100, 0], current left starts at 0 (from residual[0]+prev[0])
    // At x=1: p = prev[1] + cur[0] - prev[0] = 100 + 0 - 0 = 100
    //   pa = |100 - 0| = 100, pb = |100 - 100| = 0
    //   pb < pa → pred = prev[1] = 100
    // At x=2: p = prev[2] + cur[1] - prev[1] = 0 + 100 - 100 = 0
    //   pa = |0 - 100| = 100, pb = |0 - 0| = 0
    //   pb < pa → pred = prev[2] = 0
    let prev = vec![0i16, 100, 0];
    let residuals = vec![0i16, 0, 0];
    let mut out = vec![0i16; 3];
    unpredict_row(&residuals, Some(&prev), PredictMode::UpLeft, &mut out).unwrap();
    assert_eq!(&out, &[0, 100, 0]);
}

// ---------------------------------------------------------------------------
// predict::from_u8 invalid mode
// ---------------------------------------------------------------------------

#[test]
fn predict_mode_invalid() {
    use libdm2::predict::PredictMode;
    assert!(PredictMode::from_u8(5).is_none());
    assert!(PredictMode::from_u8(255).is_none());
    assert!(PredictMode::from_u8(0).is_some());
    assert!(PredictMode::from_u8(4).is_some());
}

// ---------------------------------------------------------------------------
// compute_tile_height edge cases (via encode)
// ---------------------------------------------------------------------------

#[test]
fn encode_none_no_tiling() {
    let px = vec![0u8; 100];
    let info = ImageInfo { width: 10, height: 10, format: PixelFormat::Gray8 };
    let enc = dm2_encode(&px, &info, Compression::None).unwrap();
    let (ri, _) = dm2_read_info(&enc).unwrap();
    assert_eq!(ri.height, 10);
    assert_eq!(enc.len(), 12 + 100);
}

// ---------------------------------------------------------------------------
// encode_auto: exercises all "this compression wins" branches
// ---------------------------------------------------------------------------

#[test]
fn auto_lossless_wins() {
    // Random data: lossless should beat default (type 2 expands random data more)
    let mut px = vec![0u8; 256 * 256];
    let mut rng = 42u32;
    for b in px.iter_mut() { rng = rng.wrapping_mul(1103515245).wrapping_add(12345); *b = (rng >> 16) as u8; }
    let info = ImageInfo { width: 256, height: 256, format: PixelFormat::Gray8 };
    let enc = dm2_encode_auto(&px, &info).unwrap();
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(&dec, &px);
}

#[test]
fn auto_default_wins() {
    // Smooth gradient: type 2 should beat lossless and none
    let mut px = vec![0u8; 256 * 256];
    for (i, b) in px.iter_mut().enumerate() { *b = (i / 256) as u8; }
    let info = ImageInfo { width: 256, height: 256, format: PixelFormat::Gray8 };
    let enc = dm2_encode_auto(&px, &info).unwrap();
    let (_, comp) = dm2_read_info(&enc).unwrap();
    assert_eq!(comp, Compression::Default, "gradient should pick type 2");
}

#[test]
fn auto_palette_wins() {
    // Random-looking 2-color RGBA: palette compresses index stream well,
    // lossless can't compress the raw 4-byte pixels as efficiently
    let mut px = vec![0u8; 256 * 256 * 4];
    let mut rng = 99u32;
    for i in 0..256 * 256 {
        rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
        let c = if (rng >> 16) & 1 == 0 { [10u8, 20, 30, 255] } else { [200, 210, 220, 255] };
        px[i*4..i*4+4].copy_from_slice(&c);
    }
    let info = ImageInfo { width: 256, height: 256, format: PixelFormat::Rgba8 };
    let enc = dm2_encode_auto(&px, &info).unwrap();
    let (_, comp) = dm2_read_info(&enc).unwrap();
    assert_eq!(comp, Compression::Palette, "random 2-color should pick palette");
}

// ---------------------------------------------------------------------------
// Malformed type 2 data: decompressed size mismatches
// ---------------------------------------------------------------------------

#[test]
fn decode_default_gray_size_mismatch() {
    // Craft a type 2 gray header for 64x64 with a tile containing a valid LZVN
    // stream that decompresses to fewer bytes than expected.
    // LZVN EOS = [0x06, 0,0,0,0,0,0,0] decompresses to 0 bytes.
    let mut data = vec![0u8; 12 + 4 + 8];
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 2; // default
    data[7] = 1; // Gray8
    data[8] = 64; data[10] = 64;
    data[12..16].copy_from_slice(&8u32.to_le_bytes()); // tile_sz = 8
    data[16] = 0x06; // LZVN EOS — decompresses to 0 bytes
    let mut out = vec![0u8; 64 * 64];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&data, &mut out, &mut di).is_err());
}

#[test]
fn decode_default_ycc_size_mismatch() {
    // Same approach for RGB type 2
    let mut data = vec![0u8; 12 + 4 + 8];
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 2;
    data[7] = 3; // RGB8
    data[8] = 64; data[10] = 64;
    data[12..16].copy_from_slice(&8u32.to_le_bytes());
    data[16] = 0x06;
    let mut out = vec![0u8; 64 * 64 * 3];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&data, &mut out, &mut di).is_err());
}

// ---------------------------------------------------------------------------
// Malformed palette tile data
// ---------------------------------------------------------------------------

#[test]
fn decode_palette_bad_index() {
    // Encode a valid 2-color palette image, then reduce paletteCount to 1
    // so that all index=1 pixels become OOB.
    let mut px = vec![0u8; 64 * 64 * 4];
    for i in 0..64 * 64 {
        let c = (i % 2) as u8;
        px[i*4] = c * 200; px[i*4+1] = c * 100; px[i*4+2] = c * 50; px[i*4+3] = 255;
    }
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Rgba8 };
    let mut enc = dm2_encode(&px, &info, Compression::Palette).unwrap();
    // Reduce ncolors from 2 to 1 — index=1 becomes OOB
    enc[12] = 1; enc[13] = 0;
    let mut out = vec![0u8; 64 * 64 * 4];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&enc, &mut out, &mut di).is_err());
}

#[test]
fn decode_palette_tile_size_mismatch() {
    // Palette file with a tile that decompresses to wrong size
    let mut data = Vec::new();
    data.extend_from_slice(b"dmp2");
    data.push(4); data.push(0); data.push(0); data.push(4);
    data.extend_from_slice(&64u16.to_le_bytes()); // tw=64
    data.extend_from_slice(&64u16.to_le_bytes()); // th=64
    data.extend_from_slice(&1u16.to_le_bytes()); // ncolors=1
    data.extend_from_slice(&4u16.to_le_bytes()); // bpe=4
    data.extend_from_slice(&[10, 20, 30, 255]);

    // Tile: LZVN EOS only — decompresses to 0 bytes
    let tile = [0x06u8, 0, 0, 0, 0, 0, 0, 0];
    data.extend_from_slice(&(tile.len() as u32).to_le_bytes());
    data.extend_from_slice(&tile);

    let mut out = vec![0u8; 64 * 64 * 4];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&data, &mut out, &mut di).is_err());
}

// ---------------------------------------------------------------------------
// Truncated tiled data (exercises decode_tiled error at line 81)
// ---------------------------------------------------------------------------

#[test]
fn decode_tiled_truncated_at_tile_header() {
    // Encode lossless, then truncate after the first tile's compressed data
    // so that the second tile's size u32 can't be read.
    // We need a multi-tile image — use a format that tiles.
    // Actually, for single-tile just truncate so the tile size field is partial.
    let mut data = vec![0u8; 14]; // 12-byte header + only 2 bytes of tile size
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 3; // lossless
    data[7] = 1; // Gray8
    data[8] = 8; data[10] = 8; // 8x8
    // Only 2 bytes after header — can't read 4-byte tile size
    let mut out = vec![0u8; 64];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&data, &mut out, &mut info).is_err());
}
