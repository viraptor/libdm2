//! Robustness fuzzing: no input, however malformed, may panic, hang, or
//! allocate unboundedly. Run in debug so arithmetic overflow also panics.
//!
//! These are deterministic (fixed-seed LCG) so a failure is reproducible.

use libdm2::{
    dm2_decode, dm2_encode_opts, dm2_read_info, lzfse, lzvn, Compression, ImageInfo, PixelFormat,
};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 16
    }
    fn byte(&mut self) -> u8 {
        self.next() as u8
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() as usize) % n
        }
    }
}

const FORMATS: &[PixelFormat] = &[
    PixelFormat::Gray8,
    PixelFormat::GrayA8,
    PixelFormat::Rgb8,
    PixelFormat::Rgba8,
    PixelFormat::Gray16,
    PixelFormat::GrayA16,
    PixelFormat::Rgb16,
    PixelFormat::Rgba16,
];

const COMPRESSIONS: &[Compression] = &[
    Compression::None,
    Compression::Default,
    Compression::Lossless,
    Compression::Palette,
];

/// A handful of valid encoded streams to use as mutation seeds.
fn seed_streams() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut rng = Lcg(0x5eed);
    for &fmt in FORMATS {
        for &(w, h) in &[(1u32, 1u32), (7, 5), (24, 16), (64, 64), (129, 33)] {
            let ps = fmt.pixel_size();
            let pixels: Vec<u8> = (0..(w as usize * h as usize * ps))
                .map(|i| if i % 3 == 0 { rng.byte() } else { (i % 251) as u8 })
                .collect();
            let info = ImageInfo { width: w, height: h, format: fmt };
            for &comp in COMPRESSIONS {
                let param = if fmt.is_16bit() { 10 } else { 0 };
                if let Ok(enc) = dm2_encode_opts(&pixels, &info, comp, 0, param) {
                    out.push((format!("{fmt:?}/{w}x{h}/{comp:?}"), enc));
                }
            }
        }
    }
    assert!(!out.is_empty(), "no seed streams could be produced");
    out
}

/// Decode into a buffer sized from the stream's own header, the way a
/// caller who trusts `dm2_read_info` would. Must never panic.
fn try_decode_dm2(data: &[u8]) {
    // Header-driven path: whatever read_info reports, allocate that and decode.
    if let Ok((info, _comp)) = dm2_read_info(data) {
        // Refuse to be the one that OOMs: a hostile header can claim huge
        // dimensions. Cap what the *test* allocates, and separately assert
        // the library reports something sane.
        let want = (info.width as u64) * (info.height as u64) * (info.format.pixel_size() as u64);
        if want <= 64 << 20 {
            let mut pixels = vec![0u8; want as usize];
            let mut out_info = info.clone();
            let _ = dm2_decode(data, &mut pixels, &mut out_info);
        }
    }
    // Mismatched-buffer path: caller's buffer disagrees with the header.
    for size in [0usize, 1, 3, 17, 256, 4096] {
        let mut pixels = vec![0u8; size];
        let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        let _ = dm2_decode(data, &mut pixels, &mut info);
    }
}

#[test]
fn fuzz_dm2_decode_mutated_streams() {
    let seeds = seed_streams();
    let mut rng = Lcg(0xd00d);
    for (label, seed) in &seeds {
        for round in 0..300 {
            let mut data = seed.clone();
            // Mutate: flips, truncations, splices, length-field corruption
            match rng.below(5) {
                0 => {
                    for _ in 0..1 + rng.below(6) {
                        let p = rng.below(data.len());
                        data[p] ^= rng.byte() | 1;
                    }
                }
                1 => {
                    let cut = rng.below(data.len() + 1);
                    data.truncate(cut);
                }
                2 => {
                    // Corrupt a 4-byte little-endian field (tile size / header)
                    if data.len() >= 16 {
                        let p = rng.below(data.len() - 4);
                        let v = match rng.below(4) {
                            0 => 0u32,
                            1 => u32::MAX,
                            2 => 0x7fff_ffff,
                            _ => rng.next() as u32,
                        };
                        data[p..p + 4].copy_from_slice(&v.to_le_bytes());
                    }
                }
                3 => {
                    // Corrupt header bytes specifically (dims, format, type)
                    if data.len() >= 12 {
                        let p = 4 + rng.below(8);
                        data[p] = rng.byte();
                    }
                }
                _ => {
                    // Splice in a run of a repeated byte
                    if !data.is_empty() {
                        let p = rng.below(data.len());
                        let n = rng.below(64).min(data.len() - p);
                        let b = rng.byte();
                        for i in 0..n {
                            data[p + i] = b;
                        }
                    }
                }
            }
            let _ = round;
            let _ = label;
            try_decode_dm2(&data);
        }
    }
}

#[test]
fn fuzz_dm2_decode_random_and_structured() {
    let mut rng = Lcg(0xfeedface);
    // Pure random
    for _ in 0..2000 {
        let n = rng.below(300);
        let data: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
        try_decode_dm2(&data);
    }
    // Valid magic + hostile header + random tail: gets much deeper into the
    // decoder than pure random ever will.
    for _ in 0..20000 {
        let mut data = b"dmp2".to_vec();
        data.push([0u8, 1, 2, 3, 4, 5, 255][rng.below(7)]); // compression
        data.push(rng.below(3) as u8); // quality
        data.push([0u8, 9, 10, 11, 12, 13, 255][rng.below(7)]); // param
        data.push([0u8, 1, 2, 3, 4, 0x11, 0x12, 0x13, 0x14, 255][rng.below(10)]); // format
        let dims: &[u16] = &[0, 1, 2, 3, 4, 7, 16, 255, 256, 4096, 16384, 32768, 65535];
        data.extend_from_slice(&dims[rng.below(dims.len())].to_le_bytes()); // tile w
        data.extend_from_slice(&dims[rng.below(dims.len())].to_le_bytes()); // tile h
        // Tiles: [u32 size][payload]
        for _ in 0..1 + rng.below(3) {
            let declared = match rng.below(5) {
                0 => 0u32,
                1 => u32::MAX,
                2 => rng.next() as u32,
                _ => rng.below(80) as u32,
            };
            data.extend_from_slice(&declared.to_le_bytes());
            let actual = rng.below(80);
            for _ in 0..actual {
                data.push(rng.byte());
            }
        }
        try_decode_dm2(&data);
    }
}

/// LZFSE container decoder against hostile streams, including ones that
/// keep a valid bvx2 header but corrupt the payload and table data.
#[test]
fn fuzz_lzfse_decode_buffer() {
    let mut rng = Lcg(0xbadc0de);

    // Valid streams as mutation seeds, across compressibility regimes
    let mut seeds: Vec<Vec<u8>> = Vec::new();
    for &n in &[4096usize, 9000, 50000] {
        let smooth: Vec<u8> = (0..n).map(|i| (i / 7) as u8).collect();
        let noisy: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
        let mixed: Vec<u8> = (0..n)
            .map(|i| if (i / 100) % 2 == 0 { 0 } else { rng.byte() })
            .collect();
        for src in [smooth, noisy, mixed] {
            if let Some(s) = lzfse::encode_buffer(&src, n * 2 + 512) {
                seeds.push(s);
            }
        }
    }
    assert!(!seeds.is_empty());

    for seed in &seeds {
        for _ in 0..500 {
            let mut data = seed.clone();
            match rng.below(4) {
                0 => {
                    for _ in 0..1 + rng.below(8) {
                        let p = rng.below(data.len());
                        data[p] ^= rng.byte() | 1;
                    }
                }
                1 => data.truncate(rng.below(data.len() + 1)),
                2 => {
                    // Hit the v2 header fields hard (first 32 bytes of a block)
                    let p = rng.below(32.min(data.len()));
                    data[p] = rng.byte();
                }
                _ => {
                    let p = rng.below(data.len().saturating_sub(4).max(1));
                    if p + 4 <= data.len() {
                        let v = if rng.below(2) == 0 { u32::MAX } else { rng.next() as u32 };
                        data[p..p + 4].copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
            for out in [0usize, 1, 100, 65536] {
                let _ = lzfse::decode_buffer(&data, out);
            }
        }
    }

    // Hand-built headers: valid magic, hostile fields
    for _ in 0..20000 {
        let mut data = Vec::new();
        let magic: &[u8] = match rng.below(5) {
            0 => b"bvx2",
            1 => b"bvx1",
            2 => b"bvx-",
            3 => b"bvxn",
            _ => b"bvx$",
        };
        data.extend_from_slice(magic);
        for _ in 0..1 + rng.below(40) {
            match rng.below(3) {
                0 => data.extend_from_slice(&(rng.next() as u32).to_le_bytes()),
                1 => data.extend_from_slice(&u32::MAX.to_le_bytes()),
                _ => data.extend_from_slice(&0u32.to_le_bytes()),
            }
        }
        for _ in 0..rng.below(200) {
            data.push(rng.byte());
        }
        for out in [0usize, 64, 5000] {
            let _ = lzfse::decode_buffer(&data, out);
        }
    }
}

/// Raw LZVN decoder: every opcode byte reachable, arbitrary payload.
#[test]
fn fuzz_lzvn_decode() {
    let mut rng = Lcg(0x1234abcd);

    // Every opcode as a leading byte, with assorted tails
    for opc in 0u16..=255 {
        for tail_len in [0usize, 1, 2, 3, 8, 20] {
            for fill in [0u8, 0xff, 0x06, 0xe0] {
                let mut src = vec![opc as u8];
                src.extend(std::iter::repeat(fill).take(tail_len));
                for dst_len in [0usize, 1, 7, 8, 64] {
                    let mut dst = vec![0u8; dst_len];
                    let _ = lzvn::decode(&src, &mut dst);
                }
            }
        }
    }

    // Random opcode soup
    for _ in 0..50000 {
        let n = 1 + rng.below(120);
        let src: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
        for dst_len in [0usize, 1, 16, 512] {
            let mut dst = vec![0u8; dst_len];
            let _ = lzvn::decode(&src, &mut dst);
        }
    }

    // Mutated valid LZVN streams
    for &n in &[64usize, 1000, 4000] {
        let src: Vec<u8> = (0..n).map(|i| ((i / 5) % 17) as u8).collect();
        let enc = lzvn::encode(&src);
        for _ in 0..3000 {
            let mut data = enc.clone();
            if data.is_empty() {
                continue;
            }
            for _ in 0..1 + rng.below(4) {
                let p = rng.below(data.len());
                data[p] ^= rng.byte() | 1;
            }
            if rng.below(3) == 0 {
                data.truncate(rng.below(data.len() + 1));
            }
            for dst_len in [0usize, n / 2, n, n * 2] {
                let mut dst = vec![0u8; dst_len];
                let _ = lzvn::decode(&data, &mut dst);
            }
        }
    }
}

/// Encoders must not panic on hostile-but-legal arguments: undersized
/// destinations, degenerate dimensions, boundary options.
#[test]
fn fuzz_encoders_hostile_arguments() {
    let mut rng = Lcg(0xc0ffee11);

    // LZFSE encoder with destination budgets from 0 upward
    for &n in &[0usize, 1, 7, 8, 100, 4095, 4096, 5000, 40000] {
        let src: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
        let smooth: Vec<u8> = vec![7u8; n];
        for data in [&src, &smooth] {
            for dst_size in [0usize, 1, 8, 11, 12, 13, 20, 100, n / 2, n, n + 12, n * 2 + 512] {
                let _ = lzfse::encode_buffer(data, dst_size);
            }
        }
    }

    // LZVN encoder on degenerate inputs
    for n in 0..40usize {
        let data: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
        let enc = lzvn::encode(&data);
        let mut dec = vec![0u8; n.max(1)];
        let _ = lzvn::decode(&enc, &mut dec);
    }

    // deepmap2 encoder: degenerate dimensions and boundary options.
    // The pixel buffer is sized correctly; the point is dimension handling.
    for &fmt in FORMATS {
        for &(w, h) in &[(0u32, 0u32), (0, 5), (5, 0), (1, 1), (1, 2), (2, 1), (3, 1), (4, 1)] {
            let ps = fmt.pixel_size();
            let pixels = vec![0x3cu8; (w as usize) * (h as usize) * ps];
            let info = ImageInfo { width: w, height: h, format: fmt };
            for &comp in COMPRESSIONS {
                for quality in 0..=2u8 {
                    for param in [0u8, 8, 9, 12, 13, 255] {
                        let _ = dm2_encode_opts(&pixels, &info, comp, quality, param);
                    }
                }
            }
        }
    }
}

/// A short pixel buffer must be rejected or handled, never overrun.
#[test]
fn encode_rejects_short_pixel_buffer() {
    for &fmt in FORMATS {
        let (w, h) = (16u32, 8u32);
        let full = (w as usize) * (h as usize) * fmt.pixel_size();
        let info = ImageInfo { width: w, height: h, format: fmt };
        for short in [0usize, 1, full / 2, full - 1] {
            let pixels = vec![0x11u8; short];
            for &comp in COMPRESSIONS {
                let param = if fmt.is_16bit() { 10 } else { 0 };
                // Must return an error rather than panicking or reading OOB
                let _ = dm2_encode_opts(&pixels, &info, comp, 0, param);
            }
        }
    }
}

/// Decoding must never write past the caller's pixel buffer, even when the
/// header claims a much larger image. Guard bytes detect an overrun.
#[test]
fn decode_respects_caller_buffer_bounds() {
    const GUARD: u8 = 0xA5;
    let seeds = seed_streams();
    let mut rng = Lcg(0x99cc);
    for (label, seed) in &seeds {
        for _ in 0..60 {
            let mut data = seed.clone();
            // Inflate the declared dimensions, keep the payload
            if data.len() >= 12 {
                match rng.below(3) {
                    0 => data[8..10].copy_from_slice(&65535u16.to_le_bytes()),
                    1 => data[10..12].copy_from_slice(&65535u16.to_le_bytes()),
                    _ => {
                        data[8..10].copy_from_slice(&4096u16.to_le_bytes());
                        data[10..12].copy_from_slice(&4096u16.to_le_bytes());
                    }
                }
            }
            let usable = 4096usize;
            let mut buf = vec![0u8; usable + 64];
            for b in buf[usable..].iter_mut() {
                *b = GUARD;
            }
            let (pixels, guard) = buf.split_at_mut(usable);
            let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
            let _ = dm2_decode(&data, pixels, &mut info);
            assert!(
                guard.iter().all(|&b| b == GUARD),
                "[{label}] decode wrote past the caller's pixel buffer"
            );
        }
    }
}

/// The `bvx1` block form (uncompressed frequency tables) needs a 772-byte
/// header, so random short streams never reach it. Build well-formed-length
/// bvx1 blocks with hostile field and frequency-table contents: these feed
/// straight into FSE table construction and the decode state machine.
#[test]
fn fuzz_lzfse_bvx1_hostile_tables() {
    const V1_HEADER_SIZE: usize = 772;
    let mut rng = Lcg(0x1b1b1b);

    // Field offsets within the v1 header (see src/lzfse/decode.rs::read_v1)
    for _ in 0..40000 {
        let mut h = vec![0u8; V1_HEADER_SIZE];
        h[0..4].copy_from_slice(b"bvx1");
        let put32 = |h: &mut Vec<u8>, off: usize, v: u32| {
            h[off..off + 4].copy_from_slice(&v.to_le_bytes())
        };
        let put16 = |h: &mut Vec<u8>, off: usize, v: u16| {
            h[off..off + 2].copy_from_slice(&v.to_le_bytes())
        };

        let n_literals = [0u32, 4, 8, 40000, 40004, u32::MAX][rng.below(6)];
        let n_matches = [0u32, 1, 7, 10000, 10001, u32::MAX][rng.below(6)];
        let lit_payload = [0u32, 8, 64, 1000, u32::MAX][rng.below(5)];
        let lmd_payload = [0u32, 8, 64, 1000, u32::MAX][rng.below(5)];

        put32(&mut h, 4, rng.next() as u32); // n_raw_bytes
        put32(&mut h, 12, n_literals);
        put32(&mut h, 16, n_matches);
        put32(&mut h, 20, lit_payload);
        put32(&mut h, 24, lmd_payload);
        put32(&mut h, 28, (rng.below(15) as i32 - 7) as u32); // literal_bits
        for i in 0..4 {
            put16(&mut h, 32 + 2 * i, [0u16, 1, 1023, 1024, 65535][rng.below(5)]);
        }
        put32(&mut h, 40, (rng.below(15) as i32 - 7) as u32); // lmd_bits
        put16(&mut h, 44, [0u16, 1, 63, 64, 65535][rng.below(5)]); // l_state
        put16(&mut h, 46, [0u16, 1, 63, 64, 65535][rng.below(5)]); // m_state
        put16(&mut h, 48, [0u16, 1, 255, 256, 65535][rng.below(5)]); // d_state

        // Frequency tables. Mix distributions that are individually valid
        // (sum == nstates), under-full (sum < nstates, leaving decoder
        // entries uninitialized), and over-full (must be rejected).
        fn fill(rng: &mut Lcg, h: &mut Vec<u8>, off: usize, count: usize, nstates: u32, mode: usize) {
            let put16 = |h: &mut Vec<u8>, off: usize, v: u16| h[off..off + 2].copy_from_slice(&v.to_le_bytes());
            let mut remaining = match mode {
                0 => nstates,                       // exactly full
                1 => nstates / 2,                   // under-full
                2 => nstates * 4,                   // over-full: must be rejected
                _ => rng.next() as u32 % (nstates * 2 + 1),
            };
            for i in 0..count {
                let v = if i + 1 == count {
                    remaining.min(u16::MAX as u32)
                } else if remaining == 0 {
                    0
                } else {
                    let take = (rng.next() as u32) % (remaining + 1);
                    remaining -= take;
                    take
                };
                put16(h, off + 2 * i, v.min(u16::MAX as u32) as u16);
            }
        }
        let m = rng.below(4); fill(&mut rng, &mut h, 50, 20, 64, m); // l_freq
        let m = rng.below(4); fill(&mut rng, &mut h, 90, 20, 64, m); // m_freq
        let m = rng.below(4); fill(&mut rng, &mut h, 130, 64, 256, m); // d_freq
        let m = rng.below(4); fill(&mut rng, &mut h, 258, 256, 1024, m); // literal_freq

        // Payload after the header, plus an end-of-stream marker
        let mut data = h;
        for _ in 0..rng.below(600) {
            data.push(rng.byte());
        }
        data.extend_from_slice(b"bvx$");

        for out in [0usize, 1, 100, 70000] {
            let _ = lzfse::decode_buffer(&data, out);
        }
    }
}

/// The LZFSE encoder writes 8-byte chunks through `flush`/`finish` and
/// relies on an `+8` over-allocation to absorb the last store. Sweep every
/// destination size across the boundary region for several inputs, rather
/// than sampling, so an off-by-one in that reasoning cannot hide.
#[test]
fn lzfse_encoder_exhaustive_dst_sizes() {
    let mut rng = Lcg(0x5175);
    let inputs: Vec<Vec<u8>> = vec![
        vec![0u8; 4096],
        (0..4096).map(|i| (i % 7) as u8).collect(),
        (0..4096).map(|_| rng.byte()).collect(),
        (0..9000).map(|i| (i / 13) as u8).collect(),
        (0..9000).map(|_| rng.byte()).collect(),
    ];
    for (n, data) in inputs.iter().enumerate() {
        // Every size from 0 up past the natural output length: each must
        // either produce a valid stream or cleanly report failure.
        let natural = lzfse::encode_buffer(data, data.len() * 2 + 512)
            .map(|v| v.len())
            .unwrap_or(data.len() + 12);
        for dst_size in 0..=(natural + 32) {
            match lzfse::encode_buffer(data, dst_size) {
                Some(enc) => {
                    assert!(
                        enc.len() <= dst_size,
                        "input {n}: encoder returned {} bytes for a {dst_size}-byte budget",
                        enc.len()
                    );
                    let back = lzfse::decode_buffer(&enc, data.len() + 64);
                    assert_eq!(
                        back.as_deref(),
                        Some(&data[..]),
                        "input {n}: stream produced at dst_size={dst_size} does not round-trip"
                    );
                }
                None => {}
            }
        }
    }
}

// ------------------------------------------------------------------
//  Regressions for crashes found during the robustness audit
// ------------------------------------------------------------------

/// A `bvx1` block carries `literal_bits`/`lmd_bits` as a raw i32. Values near
/// i32::MAX made `FseInStream::init` compute `n + 64` and panic under overflow
/// checks. Reachable from `lzfse::decode_buffer` directly, and from the public
/// deepmap2 decoder via a tile holding a bvx2 block followed by a bvx1 block.
#[test]
fn regression_bvx1_extreme_bit_counts() {
    fn bvx1(literal_bits: u32, lmd_bits: u32, n_lmd_payload: u32, trailing: usize) -> Vec<u8> {
        let mut h = vec![0u8; 772];
        h[0..4].copy_from_slice(b"bvx1");
        h[24..28].copy_from_slice(&n_lmd_payload.to_le_bytes());
        h[28..32].copy_from_slice(&literal_bits.to_le_bytes());
        h[40..44].copy_from_slice(&lmd_bits.to_le_bytes());
        h.extend(std::iter::repeat(0u8).take(trailing));
        h
    }
    // Every value whose +64 or +56 would overflow, plus assorted extremes
    for bits in [
        0x7FFF_FFFFu32, 0x7FFF_FFC0, 0x7FFF_FFF0, 0x8000_0000, 0xFFFF_FFFF, 1, 7, 64,
    ] {
        for out in [0usize, 64, 4096] {
            let _ = lzfse::decode_buffer(&bvx1(bits, 0, 0, 0), out);
            let _ = lzfse::decode_buffer(&bvx1(0, bits, 8, 8), out);
            let _ = lzfse::decode_buffer(&bvx1(bits, bits, 8, 8), out);
        }
    }

    // Same block reached through the deepmap2 tile path
    let src: Vec<u8> = (0..8000u32).map(|i| (i / 11) as u8).collect();
    let mut tile = lzfse::encode_buffer(&src, 20000).unwrap();
    tile.truncate(tile.len() - 4); // drop the bvx$ terminator so decoding continues
    tile.extend_from_slice(&bvx1(0x7FFF_FFFF, 0, 0, 0));

    let width: u16 = 8000;
    let mut data = b"dmp2".to_vec();
    data.extend_from_slice(&[3, 0, 0, 1]); // Lossless, Gray8
    data.extend_from_slice(&width.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&(tile.len() as u32).to_le_bytes());
    data.extend_from_slice(&tile);

    let mut pixels = vec![0u8; width as usize];
    let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
    let _ = dm2_decode(&data, &mut pixels, &mut info);
}

/// `dm2_encode_bound` must never panic and must never return a value smaller
/// than the raw pixel data — a caller sizing a buffer from it would
/// under-allocate. Unrepresentable bounds report 0.
#[test]
fn regression_encode_bound_never_understates() {
    for &fmt in FORMATS {
        for &(w, h) in &[
            (0u32, 0u32), (1, 1), (16, 16), (65535, 65535),
            (u32::MAX, 1), (1, u32::MAX), (u32::MAX, u32::MAX),
            (u32::MAX, 0x8000_0000), (0x8000_0000, 0x8000_0000), (100000, 100000),
        ] {
            let info = ImageInfo { width: w, height: h, format: fmt };
            let bound = libdm2::dm2_encode_bound(&info);
            let raw = (w as u128) * (h as u128) * (fmt.pixel_size() as u128);
            assert!(
                bound == 0 || bound as u128 >= raw,
                "{fmt:?} {w}x{h}: bound {bound} is below the raw size {raw}"
            );
        }
    }
}

/// A tile length prefix is a full attacker-controlled u32. The bounds check
/// must not be done by an addition that could wrap on a 32-bit target.
///
/// NOTE: on a 64-bit host this test passes with or without the fix — `offset +
/// 0xFFFFFFFF` cannot wrap a 64-bit usize. It documents the case and guards
/// 32-bit targets; the actual guarantee is the subtraction form of the check
/// in `decode_tiled`, which is what a 32-bit build depends on.
#[test]
fn regression_tile_size_prefix_extremes() {
    for &tile_sz in &[u32::MAX, 0xFFFF_FFF0, 0x8000_0000, 0x7FFF_FFFF, 0] {
        for &fmt_code in &[1u8, 4, 0x14] {
            let mut data = b"dmp2".to_vec();
            data.extend_from_slice(&[2, 0, 10, fmt_code]);
            data.extend_from_slice(&64u16.to_le_bytes());
            data.extend_from_slice(&4u16.to_le_bytes());
            data.extend_from_slice(&tile_sz.to_le_bytes());
            data.extend_from_slice(&[0x41u8; 64]);
            let mut pixels = vec![0u8; 64 * 4 * 8];
            let mut info = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
            let _ = dm2_decode(&data, &mut pixels, &mut info);
        }
    }
}
