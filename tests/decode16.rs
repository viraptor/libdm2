//! 16-bit (pixel formats 0x11..=0x14) decode tests against captured Apple
//! output.
//!
//! Each fixture pair in `tests/data/` is
//!   - `*.dm2` — a deepmap2 stream produced by Apple (`vImageDeepmap2Encode`
//!     for the synthetic ones; extracted verbatim from a real Assets.car for
//!     `*_car.dm2`), and
//!   - `*.pix` — the pixels Apple's own `vImageDeepmap2Decode` returned for
//!     that exact stream (little-endian u16 per channel; in real .car
//!     renditions these are IEEE half-float bit patterns).
//!
//! Decoding a fixed stream is deterministic, so libdm2 must reproduce the
//! `.pix` bytes EXACTLY. The synthetic type-2 fixtures are 24×16 images of
//! valid random halfs (|v| < 4) and cover, for every 16-bit format, every
//! legal `param` (9..=12 — the fixed-point scale) at quality 1 plus a
//! quality-0 sample. Runs on any host (no Accelerate needed) — the oracle
//! output is baked in.

use libdm2::*;

struct Fixture {
    name: &'static str,
    fmt: PixelFormat,
    w: u32,
    h: u32,
    dm2: &'static [u8],
    pix: &'static [u8],
}

macro_rules! fixture {
    ($name:literal, $fmt:expr, $w:expr, $h:expr) => {
        Fixture {
            name: $name,
            fmt: $fmt,
            w: $w,
            h: $h,
            dm2: include_bytes!(concat!("data/", $name, ".dm2")),
            pix: include_bytes!(concat!("data/", $name, ".pix")),
        }
    };
}

fn fixtures() -> Vec<Fixture> {
    use PixelFormat::*;
    vec![
        fixture!("rgba16_t2_q1_p9", Rgba16, 24, 16),
        fixture!("rgba16_t2_q1_p10", Rgba16, 24, 16),
        fixture!("rgba16_t2_q1_p11", Rgba16, 24, 16),
        fixture!("rgba16_t2_q1_p12", Rgba16, 24, 16),
        fixture!("rgba16_t2_q0_p10", Rgba16, 24, 16),
        fixture!("rgba16_t3_q0_p10", Rgba16, 24, 16),
        fixture!("rgba16_t3_8x8_car", Rgba16, 8, 8),
        fixture!("gray16_t2_q1_p9", Gray16, 24, 16),
        fixture!("gray16_t2_q1_p10", Gray16, 24, 16),
        fixture!("gray16_t2_q1_p11", Gray16, 24, 16),
        fixture!("gray16_t2_q1_p12", Gray16, 24, 16),
        fixture!("gray16_t2_q0_p10", Gray16, 24, 16),
        fixture!("graya16_t2_q1_p9", GrayA16, 24, 16),
        fixture!("graya16_t2_q1_p10", GrayA16, 24, 16),
        fixture!("graya16_t2_q1_p11", GrayA16, 24, 16),
        fixture!("graya16_t2_q1_p12", GrayA16, 24, 16),
        fixture!("graya16_t2_q0_p10", GrayA16, 24, 16),
        fixture!("rgb16_t2_q1_p9", Rgb16, 24, 16),
        fixture!("rgb16_t2_q1_p10", Rgb16, 24, 16),
        fixture!("rgb16_t2_q1_p11", Rgb16, 24, 16),
        fixture!("rgb16_t2_q1_p12", Rgb16, 24, 16),
        fixture!("rgb16_t2_q0_p10", Rgb16, 24, 16),
    ]
}

#[test]
fn decode16_matches_apple() {
    for f in fixtures() {
        let ps = f.fmt.pixel_size();
        let expected_len = f.w as usize * f.h as usize * ps;
        assert_eq!(f.pix.len(), expected_len, "[{}] bad fixture pix size", f.name);

        let (info, _comp) = dm2_read_info(f.dm2).expect(f.name);
        assert_eq!(info.format, f.fmt, "[{}] format", f.name);

        let mut out = vec![0u8; expected_len];
        let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        dm2_decode(f.dm2, &mut out, &mut di)
            .unwrap_or_else(|e| panic!("[{}] decode failed: {e}", f.name));
        assert_eq!(di.width, f.w, "[{}] width", f.name);
        assert_eq!(di.height, f.h, "[{}] height", f.name);
        assert_eq!(di.format, f.fmt, "[{}] out format", f.name);

        if out != f.pix {
            let first = out.iter().zip(f.pix.iter()).position(|(a, b)| a != b).unwrap();
            let px = first / ps;
            panic!(
                "[{}] pixels diverge from Apple decode: first at byte {} (px {},{}) ours={:02x?} apple={:02x?}",
                f.name,
                first,
                px % f.w as usize,
                px / f.w as usize,
                &out[first..(first + ps).min(out.len())],
                &f.pix[first..(first + ps).min(f.pix.len())]
            );
        }
    }
}

/// A 16-bit type-2 stream whose `param` is outside Apple's legal 9..=12
/// range must be rejected, not decoded with a bogus fixed-point scale.
#[test]
fn bad_param_rejected_16bit_default() {
    for f in fixtures() {
        if f.name.contains("_t3_") {
            continue; // param is irrelevant to Lossless streams
        }
        let mut stream = f.dm2.to_vec();
        let mut out = vec![0u8; f.w as usize * f.h as usize * f.fmt.pixel_size()];
        let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        for bad in [0u8, 8, 13, 255] {
            stream[6] = bad; // header param byte
            assert!(
                dm2_decode(&stream, &mut out, &mut di).is_err(),
                "[{}] param={bad} must be rejected for 16-bit type 2",
                f.name
            );
        }
    }
}
