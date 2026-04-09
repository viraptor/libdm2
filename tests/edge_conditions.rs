//! Edge-condition tests targeting boundaries and malformed input that the
//! main coverage suite does not exercise: overflow guards, palette limits,
//! truncated compressed streams, and tile-height boundaries.

use libdm2::*;

// ---------------------------------------------------------------------------
// ImageInfo::checked_raw_size — overflow boundaries
// ---------------------------------------------------------------------------

#[test]
fn checked_raw_size_overflow_huge_dimensions() {
    let info = ImageInfo {
        width: u32::MAX,
        height: u32::MAX,
        format: PixelFormat::Rgba16,
    };
    assert!(info.checked_raw_size().is_err());
}

#[test]
fn checked_raw_size_overflow_large_pixel_format() {
    // 65536 * 65536 * 8 = 2^35 — fits on 64-bit usize, overflows on 32-bit.
    let info = ImageInfo {
        width: 65536,
        height: 65536,
        format: PixelFormat::Rgba16,
    };
    #[cfg(target_pointer_width = "32")]
    assert!(info.checked_raw_size().is_err());
    #[cfg(target_pointer_width = "64")]
    assert_eq!(info.checked_raw_size().unwrap(), 1usize << 35);
}

#[test]
fn checked_raw_size_zero_dim_is_zero() {
    let info = ImageInfo { width: 0, height: 100, format: PixelFormat::Rgba8 };
    assert_eq!(info.checked_raw_size().unwrap(), 0);
    let info = ImageInfo { width: 100, height: 0, format: PixelFormat::Rgba8 };
    assert_eq!(info.checked_raw_size().unwrap(), 0);
}

#[test]
fn encode_huge_dimensions_rejected() {
    let info = ImageInfo {
        width: u32::MAX,
        height: u32::MAX,
        format: PixelFormat::Rgba16,
    };
    // Should fail cleanly, not panic.
    let result = dm2_encode(&[], &info, Compression::None);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Decode: minimum / tiny inputs
// ---------------------------------------------------------------------------

#[test]
fn decode_empty_input() {
    let mut pixels = vec![0u8; 16];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&[], &mut pixels, &mut info).is_err());
}

#[test]
fn decode_one_byte_input() {
    let mut pixels = vec![0u8; 16];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(&[0x64], &mut pixels, &mut info).is_err());
}

#[test]
fn decode_only_magic() {
    let mut pixels = vec![0u8; 16];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    assert!(dm2_decode(b"dmp2", &mut pixels, &mut info).is_err());
}

#[test]
fn read_info_empty() {
    assert!(dm2_read_info(&[]).is_err());
}

#[test]
fn read_info_truncated() {
    assert!(dm2_read_info(b"dmp2\x01").is_err());
}

// ---------------------------------------------------------------------------
// Default (type 2) compression: truncated tile size header
// ---------------------------------------------------------------------------

#[test]
fn decode_default_truncated_tile_header() {
    // valid 4x4 default header, but pixel area is too short to contain
    // even a 4-byte tile-size prefix.
    let mut data = vec![0u8; 14];
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 2; // Default
    data[7] = 1; // Gray8
    data[8] = 4; data[9] = 0; // width=4
    data[10] = 4; data[11] = 0; // height=4
    // Only 2 bytes of payload, less than the 4-byte tile size header.
    let mut out = vec![0u8; 16];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    let r = dm2_decode(&data, &mut out, &mut info);
    assert!(r.is_err(), "should error, not panic");
}

#[test]
fn decode_default_tile_size_overflow() {
    // tile_sz = u32::MAX → offset + tile_sz must not panic, must error.
    let mut data = vec![0u8; 16];
    data[0..4].copy_from_slice(b"dmp2");
    data[4] = 2; // Default
    data[7] = 1; // Gray8
    data[8] = 4; data[9] = 0;
    data[10] = 4; data[11] = 0;
    // tile size header
    data[12] = 0xFF; data[13] = 0xFF; data[14] = 0xFF; data[15] = 0xFF;
    let mut out = vec![0u8; 16];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    let r = dm2_decode(&data, &mut out, &mut info);
    assert!(r.is_err());
}

// ---------------------------------------------------------------------------
// Palette: boundary at 256 entries
// ---------------------------------------------------------------------------

#[test]
fn palette_exactly_256_distinct_colors_roundtrip() {
    // 16x16 = 256 pixels, all distinct → palette of size 256 (the max).
    let w = 16u32;
    let h = 16u32;
    let mut pixels = Vec::with_capacity(256 * 4);
    for i in 0..256u32 {
        pixels.push(i as u8);
        pixels.push((i ^ 0x55) as u8);
        pixels.push((i ^ 0xAA) as u8);
        pixels.push(0xFF);
    }
    let info = ImageInfo { width: w, height: h, format: PixelFormat::Rgba8 };
    // Force palette compression specifically.
    let enc = dm2_encode(&pixels, &info, Compression::Palette).unwrap();
    let mut dec = vec![0u8; pixels.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(dec, pixels);
}

#[test]
fn palette_257_colors_falls_back() {
    // 257 distinct colors — palette must refuse; encoder should still
    // succeed via auto, or return an error for explicit Palette.
    let w = 257u32;
    let h = 1u32;
    let mut pixels = Vec::with_capacity(257 * 4);
    for i in 0..257u32 {
        pixels.push((i & 0xFF) as u8);
        pixels.push((i >> 8) as u8);
        pixels.push(0);
        pixels.push(0xFF);
    }
    let info = ImageInfo { width: w, height: h, format: PixelFormat::Rgba8 };
    // Explicit palette: should fail (or fall through to non-palette).
    let r = dm2_encode(&pixels, &info, Compression::Palette);
    if let Ok(enc) = r {
        // If it succeeded, decoding must roundtrip.
        let mut dec = vec![0u8; pixels.len()];
        let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        dm2_decode(&enc, &mut dec, &mut di).unwrap();
        assert_eq!(dec, pixels);
    }
    // Auto must always succeed by picking a non-palette method.
    let enc = dm2_encode_auto(&pixels, &info).unwrap();
    let mut dec = vec![0u8; pixels.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(dec, pixels);
}

// ---------------------------------------------------------------------------
// Tile boundaries: heights that aren't multiples of the default tile size
// ---------------------------------------------------------------------------

fn roundtrip(w: u32, h: u32, fmt: PixelFormat, comp: Compression) {
    let ps = fmt.pixel_size();
    let mut px = vec![0u8; w as usize * h as usize * ps];
    for (i, b) in px.iter_mut().enumerate() {
        *b = ((i.wrapping_mul(31) ^ 0x5A) & 0xFF) as u8;
    }
    let info = ImageInfo { width: w, height: h, format: fmt };
    let enc = dm2_encode(&px, &info, comp).unwrap();
    let mut dec = vec![0u8; px.len()];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    dm2_decode(&enc, &mut dec, &mut di).unwrap();
    assert_eq!(dec, px);
}

#[test]
fn default_height_one_below_tile() {
    roundtrip(32, 31, PixelFormat::Rgba8, Compression::Default);
}

#[test]
fn default_height_one_above_tile() {
    roundtrip(32, 33, PixelFormat::Rgba8, Compression::Default);
}

#[test]
fn default_height_exactly_two_tiles() {
    roundtrip(32, 64, PixelFormat::Rgba8, Compression::Default);
}

#[test]
fn lossless_height_odd() {
    roundtrip(17, 17, PixelFormat::Rgb8, Compression::Lossless);
}

#[test]
fn lossless_thin_strip() {
    roundtrip(2, 2000, PixelFormat::Gray8, Compression::Lossless);
}

// ---------------------------------------------------------------------------
// Random fuzz: random bytes must never panic the decoder
// ---------------------------------------------------------------------------

#[test]
fn fuzz_random_bytes_does_not_panic() {
    // Deterministic LCG so the test is reproducible.
    let mut state: u64 = 0xDEAD_BEEF_CAFE_F00D;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };
    for _ in 0..200 {
        let len = (next() as usize) % 256;
        let mut data = vec![0u8; len];
        for b in data.iter_mut() { *b = next() as u8; }
        let mut out = vec![0u8; 4096];
        let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        let _ = dm2_decode(&data, &mut out, &mut info);
    }
}

#[test]
fn fuzz_valid_header_random_payload_does_not_panic() {
    let mut state: u64 = 0x0123_4567_89AB_CDEF;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };
    for trial in 0..200 {
        let comp = (trial % 4 + 1) as u8;
        let fmt = match trial % 8 {
            0 => 1, 1 => 2, 2 => 3, 3 => 4,
            4 => 0x11, 5 => 0x12, 6 => 0x13, _ => 0x14,
        };
        let w = (trial % 16 + 1) as u16;
        let h = (trial % 16 + 1) as u16;
        let payload_len = (next() as usize) % 64;
        let mut data = vec![0u8; 12 + payload_len];
        data[0..4].copy_from_slice(b"dmp2");
        data[4] = comp;
        data[7] = fmt;
        data[8..10].copy_from_slice(&w.to_le_bytes());
        data[10..12].copy_from_slice(&h.to_le_bytes());
        for b in &mut data[12..] { *b = next() as u8; }
        let mut out = vec![0u8; 4096];
        let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        let _ = dm2_decode(&data, &mut out, &mut info);
    }
}

// ---------------------------------------------------------------------------
// LZVN minimal-input boundaries
// ---------------------------------------------------------------------------

#[test]
fn lzvn_tiny_input_roundtrip() {
    use libdm2::lzvn::{decode, encode};
    for n in 0..16usize {
        let src: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(37)).collect();
        let compressed = encode(&src);
        let mut dec = vec![0u8; src.len() + 16];
        let dn = decode(&compressed, &mut dec).unwrap();
        assert_eq!(&dec[..dn], &src[..], "len={n}");
    }
}

#[test]
fn lzvn_decode_truncated_does_not_panic() {
    use libdm2::lzvn::{decode, encode};
    let src: Vec<u8> = (0..512).map(|i| (i as u8).wrapping_mul(7) ^ 0x33).collect();
    let compressed = encode(&src);
    // Try every possible truncation.
    for cut in 0..compressed.len() {
        let mut dec = vec![0u8; src.len() + 16];
        let _ = decode(&compressed[..cut], &mut dec);
    }
}
