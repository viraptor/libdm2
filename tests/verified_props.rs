//! Executable counterparts of the Verus specifications in `src/verified.rs`,
//! plus property-based validation of the decoder against hostile input.
//!
//! Everything here runs under plain `cargo test`, so the properties are
//! enforced on every machine even when the Verus verifier isn't available.
//! Where a property is *proved* in `src/verified.rs` the test is a
//! belt-and-braces regression check (and pins the verified code to the
//! historical formulas it replaced); where a property is *not yet proved*
//! (colorspace, LZVN, full-stream robustness) the test is the primary
//! evidence.

use libdm2::format::{Compression, ImageInfo, PixelFormat};
use libdm2::predict::{self, PredictMode};
use libdm2::{dm2_decode, dm2_encode, dm2_encode_bound, lzvn, verified};

/// Deterministic xorshift PRNG so failures are reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 32) as u8
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// ---------------------------------------------------------------------
// Zigzag: proved in verified.rs, pinned to the historical formula here.
// ---------------------------------------------------------------------

#[test]
fn zigzag_matches_historical_formula_exhaustively() {
    for x in i16::MIN..=i16::MAX {
        let historical_enc = ((x as i32 * 2) ^ ((x as i32) >> 15)) as u16;
        assert_eq!(verified::zigzag_encode(x), historical_enc, "encode({x})");
        let z = historical_enc;
        let historical_dec = ((z >> 1) as i16) ^ (-((z & 1) as i16));
        assert_eq!(verified::zigzag_decode(z), historical_dec, "decode({z})");
    }
}

#[test]
fn zigzag_bijection_exhaustive() {
    // decode ∘ encode = id over residuals, encode ∘ decode = id over codes:
    // together with the exhaustive loop this witnesses a bijection, the
    // executable analogue of lemma_zigzag_roundtrip / lemma_zigzag_bijective.
    for x in i16::MIN..=i16::MAX {
        assert_eq!(verified::zigzag_decode(verified::zigzag_encode(x)), x);
    }
    for z in u16::MIN..=u16::MAX {
        assert_eq!(verified::zigzag_encode(verified::zigzag_decode(z)), z);
    }
}

// ---------------------------------------------------------------------
// Residual adjustment: proved inverse in verified.rs for res > i16::MIN.
// ---------------------------------------------------------------------

#[test]
fn residual_adjustment_roundtrip_exhaustive() {
    for res in (i16::MIN + 1)..=i16::MAX {
        let adj = verified::adjust_residual(res);
        assert_eq!(verified::unadjust_residual(adj), res, "res={res}");
        // Full pipeline as used by the type-2 coder: adjust → zigzag →
        // unzigzag → unadjust (lemma_residual_pipeline_roundtrip).
        let z = verified::zigzag_encode(adj);
        assert_eq!(
            verified::unadjust_residual(verified::zigzag_decode(z)),
            res,
            "pipeline res={res}"
        );
    }
    // unadjust_residual must be total: i16::MIN can arrive from hostile
    // zigzag codes and must not overflow.
    assert_eq!(verified::unadjust_residual(i16::MIN), i16::MIN + 1);
}

// ---------------------------------------------------------------------
// wrap_add_i16: verified against vstd's trusted wrapping_add spec; here
// we pin it to the independent mask/modular formulation so a regression
// in either direction is caught executably.
// ---------------------------------------------------------------------

fn wrap_add_reference(a: i16, b: i16) -> i16 {
    (((a as u16 as u32) + (b as u16 as u32)) & 0xffff) as u16 as i16
}

#[test]
fn wrap_add_matches_wrapping_add() {
    let edges = [
        i16::MIN,
        i16::MIN + 1,
        -256,
        -255,
        -2,
        -1,
        0,
        1,
        2,
        254,
        255,
        256,
        i16::MAX - 1,
        i16::MAX,
    ];
    for &a in &edges {
        for &b in &edges {
            assert_eq!(verified::wrap_add_i16(a, b), wrap_add_reference(a, b), "{a}+{b}");
        }
    }
    let mut rng = Rng::new(0x5eed);
    for _ in 0..1_000_000 {
        let a = rng.next() as i16;
        let b = (rng.next() >> 16) as i16;
        assert_eq!(verified::wrap_add_i16(a, b), wrap_add_reference(a, b), "{a}+{b}");
    }
}

// ---------------------------------------------------------------------
// Row reconstruction: verified twins vs production unpredict_row.
// ---------------------------------------------------------------------

#[test]
fn unpredict_verified_matches_production() {
    let mut rng = Rng::new(0xdec0de);
    for trial in 0..2000 {
        let w = 1 + rng.below(64);
        // Mix realistic residuals with extremes that exercise wrap-around.
        let residuals: Vec<i16> = (0..w)
            .map(|_| match rng.below(8) {
                0 => i16::MIN,
                1 => i16::MAX,
                2 => -1,
                3 => 0,
                _ => rng.next() as i16,
            })
            .collect();
        let prev: Vec<i16> = (0..w).map(|_| rng.next() as i16).collect();
        let mut out = vec![0i16; w];

        predict::unpredict_row(&residuals, None, PredictMode::None, &mut out).unwrap();
        assert_eq!(out, verified::unpredict_none(&residuals), "None trial={trial}");

        predict::unpredict_row(&residuals, Some(&prev), PredictMode::Left, &mut out).unwrap();
        assert_eq!(out, verified::unpredict_left(&residuals), "Left trial={trial}");

        predict::unpredict_row(&residuals, Some(&prev), PredictMode::Up, &mut out).unwrap();
        assert_eq!(out, verified::unpredict_up(&residuals, &prev), "Up trial={trial}");
    }
}

// ---------------------------------------------------------------------
// Colorspace: not yet Verus-proved (signed truncating division), so the
// exhaustive test is the primary evidence that the modified-YCoCg
// transform in color.rs is lossless over the full 8-bit cube.
// ---------------------------------------------------------------------

#[test]
fn ycocg_rgb_roundtrip_exhaustive_cube() {
    use libdm2::color::{rgb_to_ycocg, ycocg_to_rgb};
    let mut pixels = Vec::with_capacity(256 * 3);
    let mut y = vec![0i16; 256];
    let mut co = vec![0i16; 256];
    let mut cg = vec![0i16; 256];
    let mut out = vec![0u8; 256 * 3];
    for r in 0..=255u8 {
        for g in 0..=255u8 {
            pixels.clear();
            for b in 0..=255u8 {
                pixels.extend_from_slice(&[r, g, b]);
            }
            rgb_to_ycocg(&pixels, 256, &mut y, &mut co, &mut cg);
            ycocg_to_rgb(&y, &co, &cg, 256, &mut out);
            assert_eq!(out, pixels, "r={r} g={g}");
        }
    }
}

// ---------------------------------------------------------------------
// LZVN: robustness of the pure-Rust decoder against arbitrary input, and
// roundtrip through our encoder. This is the highest-value future Verus
// target (it parses untrusted bytes); until then, fuzz-style coverage.
// ---------------------------------------------------------------------

#[test]
fn lzvn_decode_arbitrary_input_no_panic() {
    let mut rng = Rng::new(0x1234_5678);
    let mut dst = vec![0u8; 4096];
    for _ in 0..20_000 {
        let len = rng.below(300);
        let src: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        // Must return, never panic; result value is unconstrained.
        let _ = lzvn::decode(&src, &mut dst);
    }
}

#[test]
fn lzvn_decode_truncated_and_corrupted_streams_no_panic() {
    let mut rng = Rng::new(0xc0ffee);
    let data: Vec<u8> = (0..2048).map(|_| (rng.next() % 7) as u8).collect();
    let valid = lzvn::encode(&data);
    let mut dst = vec![0u8; data.len()];

    // Every truncation point.
    for cut in 0..valid.len() {
        let _ = lzvn::decode(&valid[..cut], &mut dst);
    }
    // Every single-byte value at every position in a prefix window, plus
    // random multi-byte corruption over the whole stream.
    for pos in 0..valid.len().min(64) {
        for val in 0..=255u8 {
            let mut corrupt = valid.clone();
            corrupt[pos] = val;
            let _ = lzvn::decode(&corrupt, &mut dst);
        }
    }
    for _ in 0..20_000 {
        let mut corrupt = valid.clone();
        for _ in 0..1 + rng.below(8) {
            let p = rng.below(corrupt.len());
            corrupt[p] = rng.byte();
        }
        let _ = lzvn::decode(&corrupt, &mut dst);
    }
}

#[test]
fn lzvn_decode_undersized_output_no_panic() {
    // Valid stream, output buffer smaller than the original data: the
    // decoder is documented to truncate like Apple's.
    let data: Vec<u8> = (0..=255u8).cycle().take(3000).collect();
    let valid = lzvn::encode(&data);
    for out_len in [0usize, 1, 7, 100, 2999] {
        let mut dst = vec![0u8; out_len];
        if let Some(n) = lzvn::decode(&valid, &mut dst) {
            assert!(n <= out_len);
            assert_eq!(&dst[..n], &data[..n]);
        }
    }
}

#[test]
fn lzvn_roundtrip_many_sizes() {
    let mut rng = Rng::new(0xabcdef);
    for len in (0..64).chain([100, 500, 1000, 4095, 4096, 5000, 20000]) {
        // Compressible-ish content
        let data: Vec<u8> = (0..len).map(|i| ((i / 7) % 251) as u8).collect();
        let enc = lzvn::encode(&data);
        let mut dec = vec![0u8; len.max(1)];
        let n = lzvn::decode(&enc, &mut dec).expect("valid stream must decode");
        assert_eq!(n, len, "len={len}");
        assert_eq!(&dec[..n], &data[..], "len={len}");

        // Incompressible content
        let data: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        let enc = lzvn::encode(&data);
        let mut dec = vec![0u8; len.max(1)];
        let n = lzvn::decode(&enc, &mut dec).expect("valid stream must decode");
        assert_eq!(n, len, "random len={len}");
        assert_eq!(&dec[..n], &data[..], "random len={len}");
    }
}

// ---------------------------------------------------------------------
// Container-level robustness: dm2_decode on corrupted streams must fail
// cleanly (Err), never panic, and never write outside the output buffer
// (guaranteed by safe Rust, but exercised here anyway).
// ---------------------------------------------------------------------

fn make_image(rng: &mut Rng, w: u32, h: u32, format: PixelFormat, gradient: bool) -> Vec<u8> {
    let n = (w * h) as usize * format.pixel_size();
    (0..n)
        .map(|i| {
            if gradient {
                ((i as u32 * 7 / (w * 4).max(1)) % 256) as u8
            } else {
                rng.byte()
            }
        })
        .collect()
}

#[test]
fn decode_corrupted_streams_no_panic() {
    let mut rng = Rng::new(0xbadf00d);
    let formats = [
        PixelFormat::Gray8,
        PixelFormat::GrayA8,
        PixelFormat::Rgb8,
        PixelFormat::Rgba8,
    ];
    let compressions = [
        Compression::None,
        Compression::Default,
        Compression::Lossless,
        Compression::Palette,
    ];
    for &format in &formats {
        for &compression in &compressions {
            let info = ImageInfo { width: 40, height: 30, format };
            let pixels = make_image(&mut rng, 40, 30, format, true);
            let Ok(encoded) = dm2_encode(&pixels, &info, compression) else {
                continue; // e.g. palette rejects non-RGBA
            };

            let mut out = vec![0u8; pixels.len()];
            let mut info_out = info.clone();

            // Every single-byte corruption of the header region, plus the
            // tile-size fields.
            for pos in 0..encoded.len().min(24) {
                for val in [0u8, 1, 2, 0x7f, 0x80, 0xff] {
                    let mut corrupt = encoded.clone();
                    corrupt[pos] = val;
                    let _ = dm2_decode(&corrupt, &mut out, &mut info_out);
                }
            }
            // Every truncation length.
            for cut in 0..encoded.len().min(200) {
                let _ = dm2_decode(&encoded[..cut], &mut out, &mut info_out);
            }
            // Random corruption anywhere in the stream.
            for _ in 0..2000 {
                let mut corrupt = encoded.clone();
                for _ in 0..1 + rng.below(6) {
                    let p = rng.below(corrupt.len());
                    corrupt[p] = rng.byte();
                }
                let _ = dm2_decode(&corrupt, &mut out, &mut info_out);
            }
        }
    }
}

#[test]
fn decode_random_garbage_no_panic() {
    let mut rng = Rng::new(0xfeedface);
    let mut out = vec![0u8; 64 * 64 * 4];
    let mut info_out = ImageInfo { width: 0, height: 0, format: PixelFormat::Rgba8 };
    for _ in 0..20_000 {
        let len = rng.below(256);
        let mut buf: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        // Half the time, give it a valid magic so it gets past the first check.
        if len >= 12 && rng.below(2) == 0 {
            buf[0..4].copy_from_slice(b"dmp2");
        }
        let _ = dm2_decode(&buf, &mut out, &mut info_out);
    }
}

// ---------------------------------------------------------------------
// Encoder contracts.
// ---------------------------------------------------------------------

#[test]
fn encode_bound_holds_for_all_modes() {
    let mut rng = Rng::new(0xb0a7);
    let formats = [
        PixelFormat::Gray8,
        PixelFormat::GrayA8,
        PixelFormat::Rgb8,
        PixelFormat::Rgba8,
        PixelFormat::Gray16,
        PixelFormat::Rgba16,
    ];
    let compressions = [
        Compression::None,
        Compression::Default,
        Compression::Lossless,
        Compression::Palette,
    ];
    for &format in &formats {
        for (w, h) in [(1u32, 1u32), (3, 5), (64, 64), (257, 3)] {
            let info = ImageInfo { width: w, height: h, format };
            let bound = dm2_encode_bound(&info);
            for &compression in &compressions {
                for gradient in [true, false] {
                    let pixels = make_image(&mut rng, w, h, format, gradient);
                    if let Ok(encoded) = dm2_encode(&pixels, &info, compression) {
                        assert!(
                            encoded.len() <= bound,
                            "bound violated: {format:?} {compression:?} {w}x{h} \
                             gradient={gradient}: {} > {bound}",
                            encoded.len()
                        );
                    }
                }
            }
        }
    }
}

/// Type-2 roundtrip with pixel content chosen to hit extreme Co/Cg
/// residuals and all prediction modes — the exact code paths the verified
/// residual pipeline covers.
#[test]
fn type2_roundtrip_extreme_pixels() {
    let mut rng = Rng::new(0x7e57);
    for format in [PixelFormat::Gray8, PixelFormat::GrayA8, PixelFormat::Rgb8, PixelFormat::Rgba8]
    {
        let (w, h) = (48u32, 48u32); // big enough for the type-2 path
        let ps = format.pixel_size();
        let n = (w * h) as usize * ps;
        // Alternate saturated channel extremes to maximize |Co|/|Cg|.
        let mut pixels = vec![0u8; n];
        for (i, p) in pixels.iter_mut().enumerate() {
            *p = match rng.below(5) {
                0 => 0,
                1 => 255,
                2 => if i % ps == 0 { 255 } else { 0 }, // pure red-ish
                3 => if i % ps == 1 { 255 } else { 0 }, // pure green-ish
                _ => rng.byte(),
            };
        }
        let info = ImageInfo { width: w, height: h, format };
        let encoded = dm2_encode(&pixels, &info, Compression::Default).unwrap();
        let mut out = vec![0u8; n];
        let mut info_out = info.clone();
        dm2_decode(&encoded, &mut out, &mut info_out).unwrap();
        assert_eq!(out, pixels, "{format:?}");
    }
}
