//! RGBA16 (pixel format 0x14) decode tests against captured Apple output.
//!
//! Each fixture pair in `tests/data/` is
//!   - `*.dm2` — a deepmap2 stream produced by Apple (`vImageDeepmap2Encode`
//!     for the synthetic ones; extracted verbatim from a real Assets.car for
//!     `*_car.dm2`), and
//!   - `*.pix` — the pixels Apple's own `vImageDeepmap2Decode` returned for
//!     that exact stream (RGBA16: four little-endian u16 per pixel; in real
//!     .car renditions these are IEEE half-float bit patterns).
//!
//! Decoding a fixed stream is deterministic, so libdm2 must reproduce the
//! `.pix` bytes EXACTLY. The synthetic type-2 fixtures cover every legal
//! 16-bit `param` (9..=12 — the fixed-point scale) and quality 0/1.
//! Runs on any host (no Accelerate needed) — the oracle output is baked in.

use libdm2::*;

struct Fixture {
    name: &'static str,
    w: u32,
    h: u32,
    dm2: &'static [u8],
    pix: &'static [u8],
}

macro_rules! fixture {
    ($name:literal, $w:expr, $h:expr) => {
        Fixture {
            name: $name,
            w: $w,
            h: $h,
            dm2: include_bytes!(concat!("data/", $name, ".dm2")),
            pix: include_bytes!(concat!("data/", $name, ".pix")),
        }
    };
}

fn fixtures() -> Vec<Fixture> {
    vec![
        fixture!("rgba16_t2_q1_p9", 24, 16),
        fixture!("rgba16_t2_q1_p10", 24, 16),
        fixture!("rgba16_t2_q1_p11", 24, 16),
        fixture!("rgba16_t2_q1_p12", 24, 16),
        fixture!("rgba16_t2_q0_p10", 24, 16),
        fixture!("rgba16_t3_q0_p10", 24, 16),
        fixture!("rgba16_t3_8x8_car", 8, 8),
    ]
}

#[test]
fn rgba16_decode_matches_apple() {
    for f in fixtures() {
        let expected_len = f.w as usize * f.h as usize * 8;
        assert_eq!(f.pix.len(), expected_len, "[{}] bad fixture pix size", f.name);

        let (info, _comp) = dm2_read_info(f.dm2).expect(f.name);
        assert_eq!(info.format, PixelFormat::Rgba16, "[{}] format", f.name);

        let mut out = vec![0u8; expected_len];
        let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        dm2_decode(f.dm2, &mut out, &mut di)
            .unwrap_or_else(|e| panic!("[{}] decode failed: {e}", f.name));
        assert_eq!(di.width, f.w, "[{}] width", f.name);
        assert_eq!(di.height, f.h, "[{}] height", f.name);
        assert_eq!(di.format, PixelFormat::Rgba16, "[{}] out format", f.name);

        if out != f.pix {
            let first = out.iter().zip(f.pix.iter()).position(|(a, b)| a != b).unwrap();
            let px = first / 8;
            panic!(
                "[{}] pixels diverge from Apple decode: first at byte {} (px {},{}) ours={:02x?} apple={:02x?}",
                f.name,
                first,
                px % f.w as usize,
                px / f.w as usize,
                &out[first..(first + 8).min(out.len())],
                &f.pix[first..(first + 8).min(f.pix.len())]
            );
        }
    }
}

/// A 16-bit type-2 stream whose `param` is outside Apple's legal 9..=12
/// range must be rejected, not decoded with a bogus fixed-point scale.
#[test]
fn rgba16_default_bad_param_rejected() {
    let f = &fixtures()[1]; // rgba16_t2_q1_p10
    let mut stream = f.dm2.to_vec();
    let mut out = vec![0u8; f.w as usize * f.h as usize * 8];
    let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    for bad in [0u8, 8, 13, 255] {
        stream[6] = bad; // header param byte
        assert!(
            dm2_decode(&stream, &mut out, &mut di).is_err(),
            "param={bad} must be rejected for RGBA16 type 2"
        );
    }
}

/// The other 16-bit formats' type-2 layout is still un-reverse-engineered:
/// decoding must fail cleanly (BadFormat), never emit garbage.
#[test]
fn non_rgba16_16bit_default_rejected() {
    let f = &fixtures()[1]; // rgba16_t2_q1_p10
    for (fmt, ps) in [(0x11u8, 2usize), (0x12, 4), (0x13, 6)] {
        let mut stream = f.dm2.to_vec();
        stream[7] = fmt; // header pixel-format byte
        let mut out = vec![0u8; f.w as usize * f.h as usize * ps];
        let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        assert!(
            dm2_decode(&stream, &mut out, &mut di).is_err(),
            "16-bit format {fmt:#x} type 2 must be rejected (no spec yet)"
        );
    }
}
