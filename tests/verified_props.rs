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
// Row reconstruction: production unpredict_row (which dispatches to the
// verified implementations) vs an independent reference implementation
// preserving the pre-verification code for all five modes.
// ---------------------------------------------------------------------

fn unpredict_row_reference(
    residuals: &[i16],
    prev_row: Option<&[i16]>,
    mode: PredictMode,
    out: &mut [i16],
) {
    let w = residuals.len();
    match mode {
        PredictMode::None => out[..w].copy_from_slice(residuals),
        PredictMode::Left => {
            out[0] = residuals[0];
            for i in 1..w {
                out[i] = residuals[i].wrapping_add(out[i - 1]);
            }
        }
        PredictMode::Up => {
            let prev = prev_row.unwrap();
            for i in 0..w {
                out[i] = residuals[i].wrapping_add(prev[i]);
            }
        }
        PredictMode::UpLeft => {
            let prev = prev_row.unwrap();
            out[0] = residuals[0].wrapping_add(prev[0]);
            for i in 1..w {
                let p = prev[i] as i32 + out[i - 1] as i32 - prev[i - 1] as i32;
                let pa = (p - out[i - 1] as i32).unsigned_abs();
                let pb = (p - prev[i] as i32).unsigned_abs();
                let pred = if pb < pa { prev[i] } else { out[i - 1] };
                out[i] = residuals[i].wrapping_add(pred);
            }
        }
        PredictMode::Mean => {
            let prev = prev_row.unwrap();
            out[0] = residuals[0].wrapping_add(prev[0]);
            for i in 1..w {
                let mut sum = out[i - 1] as i32 + prev[i] as i32 + 1;
                if sum < 0 {
                    sum += 1;
                }
                let pred = (sum >> 1) as i16;
                out[i] = residuals[i].wrapping_add(pred);
            }
        }
    }
}

#[test]
fn unpredict_verified_matches_reference_all_modes() {
    let mut rng = Rng::new(0xdec0de);
    let modes = [
        PredictMode::None,
        PredictMode::Left,
        PredictMode::Up,
        PredictMode::UpLeft,
        PredictMode::Mean,
    ];
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
        let prev: Vec<i16> = (0..w)
            .map(|_| match rng.below(8) {
                0 => i16::MIN,
                1 => i16::MAX,
                _ => rng.next() as i16,
            })
            .collect();
        for mode in modes {
            let mut out = vec![0i16; w];
            let mut expected = vec![0i16; w];
            predict::unpredict_row(&residuals, Some(&prev), mode, &mut out).unwrap();
            unpredict_row_reference(&residuals, Some(&prev), mode, &mut expected);
            assert_eq!(out, expected, "{mode:?} trial={trial}");
        }
    }
}

// ---------------------------------------------------------------------
// Gray row coding: executable mirror of verified::lemma_gray_row_roundtrip
// (decode ∘ encode == id for u8-range rows, all encoder modes), plus
// hostile-input totality of decode_gray_row.
// ---------------------------------------------------------------------

#[test]
fn gray_row_roundtrip_executable() {
    let mut rng = Rng::new(0x9147);
    for trial in 0..3000 {
        let w = 1 + rng.below(48);
        let cur: Vec<i16> = (0..w).map(|_| rng.byte() as i16).collect();
        let prev: Vec<i16> = (0..w).map(|_| rng.byte() as i16).collect();
        for mode in [PredictMode::None, PredictMode::Left, PredictMode::Up] {
            let mut hi = vec![0u8; w];
            let mut lo = vec![0u8; w];
            verified::encode_gray_row(&cur, &prev, mode, &mut hi, &mut lo);
            let mut out = vec![0i16; w];
            assert!(verified::decode_gray_row(&hi, &lo, Some(&prev), mode, &mut out));
            assert_eq!(out, cur, "{mode:?} trial={trial}");
        }
    }
}

#[test]
fn decode_gray_row_hostile_bytes_total() {
    let mut rng = Rng::new(0x707a1);
    for _ in 0..5000 {
        let w = 1 + rng.below(32);
        let hi: Vec<u8> = (0..w).map(|_| rng.byte()).collect();
        let lo: Vec<u8> = (0..w).map(|_| rng.byte()).collect();
        let prev: Vec<i16> = (0..w).map(|_| rng.next() as i16).collect();
        let mut out = vec![0i16; w];
        for mode_byte in 0..=4u8 {
            let mode = PredictMode::from_u8(mode_byte).unwrap();
            // With a previous row: always succeeds, never panics.
            assert!(verified::decode_gray_row(&hi, &lo, Some(&prev), mode, &mut out));
            // Without: fails cleanly exactly for the prev-needing modes.
            let expect_ok = matches!(mode, PredictMode::None | PredictMode::Left);
            assert_eq!(
                verified::decode_gray_row(&hi, &lo, None, mode, &mut out),
                expect_ok
            );
        }
    }
}

// ---------------------------------------------------------------------
// Verified YCoCg scalar transform (used by the type-2 encoder): the
// roundtrip is proved in verified.rs; here we pin it executably over the
// full 8-bit cube and against the reference formulas from deepmap2.md.
// ---------------------------------------------------------------------

#[test]
fn ycocg_pixel_roundtrip_exhaustive_cube() {
    for r in 0..=255u8 {
        for g in 0..=255u8 {
            for b in 0..=255u8 {
                let (y, co, cg) = verified::ycocg_forward_pixel(r, g, b);
                // Reference forward per deepmap2.md
                let co_ref = r as i16 - b as i16;
                let t_ref = b as i16 + co_ref / 2;
                let cg_ref = g as i16 - t_ref;
                let y_ref = t_ref + cg_ref / 2;
                assert_eq!((y, co, cg), (y_ref, co_ref, cg_ref), "fwd r={r} g={g} b={b}");

                let (r2, g2, b2) = verified::ycocg_inverse_pixel(y, co, cg);
                assert_eq!(
                    (r2, g2, b2),
                    (r as i16, g as i16, b as i16),
                    "roundtrip r={r} g={g} b={b}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------
// Tile geometry: production compute_tile_height (verified budget math)
// vs the original reference implementation.
// ---------------------------------------------------------------------

#[test]
fn tile_height_matches_reference() {
    use libdm2::format::compute_tile_height;
    fn reference(compression: Compression, width: u32, height: u32, pixel_size: usize) -> u32 {
        let budget: usize = match compression {
            Compression::None => return height,
            Compression::Default | Compression::Palette => 1_044_480,
            Compression::Lossless => 2_097_152,
        };
        let row_bytes = width as usize * pixel_size;
        if row_bytes == 0 {
            return height;
        }
        let max_rows = budget / row_bytes;
        if max_rows == 0 {
            1
        } else {
            height.min(max_rows as u32)
        }
    }
    let compressions = [
        Compression::None,
        Compression::Default,
        Compression::Lossless,
        Compression::Palette,
    ];
    let dims = [0u32, 1, 2, 3, 64, 255, 4096, 65535, 1_000_000, u32::MAX];
    for &c in &compressions {
        for &w in &dims {
            for &h in &dims {
                for ps in [1usize, 2, 3, 4, 6, 8] {
                    assert_eq!(
                        compute_tile_height(c, w, h, ps),
                        reference(c, w, h, ps),
                        "{c:?} w={w} h={h} ps={ps}"
                    );
                }
            }
        }
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

/// The pre-verification decoder, kept verbatim as a reference oracle for
/// differential testing against the verified production decoder.
mod lzvn_reference {
    fn extract(val: u8, lsb: u8, width: u8) -> u8 {
        (val >> lsb) & ((1 << width) - 1)
    }
    fn load2(src: &[u8]) -> u16 {
        u16::from_le_bytes([src[0], src[1]])
    }
    #[derive(Clone, Copy)]
    enum Op { SmlD, MedD, LrgD, PreD, SmlM, LrgM, SmlL, LrgL, Eos, Nop, Undef }
    fn opcode_type(opc: u8) -> Op {
        #[rustfmt::skip]
        const TABLE: [u8; 256] = [
            0,0,0,0,0,0,8,3, 0,0,0,0,0,0,9,3,
            0,0,0,0,0,0,9,3, 0,0,0,0,0,0,10,3,
            0,0,0,0,0,0,10,3, 0,0,0,0,0,0,10,3,
            0,0,0,0,0,0,10,3, 0,0,0,0,0,0,10,3,
            0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3,
            0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3,
            0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3,
            10,10,10,10,10,10,10,10, 10,10,10,10,10,10,10,10,
            0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3,
            0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3,
            1,1,1,1,1,1,1,1, 1,1,1,1,1,1,1,1,
            1,1,1,1,1,1,1,1, 1,1,1,1,1,1,1,1,
            0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3,
            10,10,10,10,10,10,10,10, 10,10,10,10,10,10,10,10,
            7,6,6,6,6,6,6,6, 6,6,6,6,6,6,6,6,
            5,2,2,2,2,2,2,2, 2,2,2,2,2,2,2,2,
        ];
        match TABLE[opc as usize] {
            0 => Op::SmlD, 1 => Op::MedD, 2 => Op::SmlM, 3 => Op::LrgD,
            4 => Op::PreD, 5 => Op::LrgM, 6 => Op::SmlL, 7 => Op::LrgL,
            8 => Op::Eos, 9 => Op::Nop, _ => Op::Undef,
        }
    }
    fn copy_literal(src: &[u8], sp: &mut usize, dst: &mut [u8], dp: &mut usize, l: usize) -> Option<()> {
        if *sp + l > src.len() { return None; }
        let l = l.min(dst.len() - *dp);
        dst[*dp..*dp + l].copy_from_slice(&src[*sp..*sp + l]);
        *sp += l;
        *dp += l;
        Some(())
    }
    fn copy_match(dst: &mut [u8], dp: &mut usize, d: usize, m: usize) -> Option<()> {
        if d == 0 || d > *dp { return None; }
        let m = m.min(dst.len() - *dp);
        for i in 0..m {
            dst[*dp + i] = dst[*dp + i - d];
        }
        *dp += m;
        Some(())
    }
    pub fn decode(src: &[u8], dst: &mut [u8]) -> Option<usize> {
        let mut sp = 0usize;
        let mut dp = 0usize;
        let mut d_prev: usize = 0;
        loop {
            if sp >= src.len() { return None; }
            if dp >= dst.len() { return Some(dp); }
            let opc = src[sp];
            let src_rem = src.len() - sp;
            match opcode_type(opc) {
                Op::Eos => return Some(dp),
                Op::Nop => { sp += 1; }
                Op::Undef => return None,
                Op::SmlD => {
                    let l = extract(opc, 6, 2) as usize;
                    let m = extract(opc, 3, 3) as usize + 3;
                    if src_rem <= 2 + l { return None; }
                    let d = ((extract(opc, 0, 3) as usize) << 8) | src[sp + 1] as usize;
                    sp += 2;
                    copy_literal(src, &mut sp, dst, &mut dp, l)?;
                    copy_match(dst, &mut dp, d, m)?;
                    d_prev = d;
                }
                Op::MedD => {
                    let l = extract(opc, 3, 2) as usize;
                    if src_rem <= 3 + l { return None; }
                    let opc23 = load2(&src[sp + 1..]);
                    let m = ((extract(opc, 0, 3) as usize) << 2 | (opc23 & 3) as usize) + 3;
                    let d = (opc23 >> 2) as usize;
                    sp += 3;
                    copy_literal(src, &mut sp, dst, &mut dp, l)?;
                    copy_match(dst, &mut dp, d, m)?;
                    d_prev = d;
                }
                Op::LrgD => {
                    let l = extract(opc, 6, 2) as usize;
                    let m = extract(opc, 3, 3) as usize + 3;
                    if src_rem <= 3 + l { return None; }
                    let d = load2(&src[sp + 1..]) as usize;
                    sp += 3;
                    copy_literal(src, &mut sp, dst, &mut dp, l)?;
                    copy_match(dst, &mut dp, d, m)?;
                    d_prev = d;
                }
                Op::PreD => {
                    let l = extract(opc, 6, 2) as usize;
                    let m = extract(opc, 3, 3) as usize + 3;
                    if src_rem <= 1 + l { return None; }
                    sp += 1;
                    copy_literal(src, &mut sp, dst, &mut dp, l)?;
                    copy_match(dst, &mut dp, d_prev, m)?;
                }
                Op::SmlM => {
                    let m = extract(opc, 0, 4) as usize;
                    if src_rem <= 1 { return None; }
                    sp += 1;
                    copy_match(dst, &mut dp, d_prev, m)?;
                }
                Op::LrgM => {
                    if src_rem <= 2 { return None; }
                    let m = src[sp + 1] as usize + 16;
                    sp += 2;
                    copy_match(dst, &mut dp, d_prev, m)?;
                }
                Op::SmlL => {
                    let l = extract(opc, 0, 4) as usize;
                    if src_rem <= 1 + l { return None; }
                    sp += 1;
                    copy_literal(src, &mut sp, dst, &mut dp, l)?;
                }
                Op::LrgL => {
                    if src_rem <= 2 { return None; }
                    let l = src[sp + 1] as usize + 16;
                    if src_rem <= 2 + l { return None; }
                    sp += 2;
                    copy_literal(src, &mut sp, dst, &mut dp, l)?;
                }
            }
        }
    }
}

/// The verified production decoder must agree with the reference decoder
/// exactly — same Some/None result and same output bytes — on valid,
/// corrupted, truncated, and random streams.
#[test]
fn lzvn_verified_decoder_matches_reference() {
    let mut rng = Rng::new(0x1f2e3d);

    let check = |src: &[u8], out_len: usize| {
        let mut a = vec![0u8; out_len];
        let mut b = vec![0u8; out_len];
        let ra = lzvn::decode(src, &mut a);
        let rb = lzvn_reference::decode(src, &mut b);
        assert_eq!(ra, rb, "result mismatch for src={src:?} out_len={out_len}");
        if let Some(n) = ra {
            assert_eq!(a[..n], b[..n], "output mismatch for src={src:?}");
        }
    };

    // Valid streams of assorted content/sizes.
    for len in [0usize, 1, 7, 100, 1000, 5000] {
        let data: Vec<u8> = (0..len).map(|i| ((i * 31) % 253) as u8).collect();
        let enc = libdm2::lzvn::encode(&data);
        check(&enc, len.max(1));
        check(&enc, len / 2 + 1); // undersized output
        // Every truncation of a moderate stream.
        if enc.len() < 600 {
            for cut in 0..enc.len() {
                check(&enc[..cut], len.max(1));
            }
        }
        // Corruptions.
        for _ in 0..2000 {
            let mut c = enc.clone();
            for _ in 0..1 + rng.below(4) {
                let p = rng.below(c.len());
                c[p] = rng.byte();
            }
            check(&c, len.max(1));
        }
    }

    // Pure random garbage.
    for _ in 0..20_000 {
        let len = rng.below(200);
        let src: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        check(&src, 512);
    }
}

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
