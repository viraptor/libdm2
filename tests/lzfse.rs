//! Regression tests for the LZFSE port.
//!
//! Byte-parity was established in two stages. First against the native
//! liblzfse (see git history: tests/lzfse_parity.rs) — encode identity on
//! every container path, decode identity on valid, corrupted and truncated
//! streams. Then the encoder was adjusted to libcompression's variant of
//! the algorithm (see deepmap2.md), which is what vImage actually calls;
//! that behavior is pinned by the full-stream identity tests in
//! tests/cross_validate.rs, which compare against `vImageDeepmap2Encode`
//! directly. The golden checksums below simply pin current output so
//! refactors cannot silently change it.

use libdm2::lzfse::{decode_buffer, encode_buffer};

struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1103515245).wrapping_add(12345);
        self.0
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 16) as u8
    }
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn pattern(kind: &str, size: usize, seed: u32) -> Vec<u8> {
    match kind {
        "zeros" => vec![0u8; size],
        "cycle" => (0..size).map(|i| (i % 251) as u8).collect(),
        "random" => {
            let mut rng = Lcg(seed);
            (0..size).map(|_| rng.byte()).collect()
        }
        "text" => {
            let mut text = Vec::with_capacity(size);
            let mut rng = Lcg(seed ^ 0xdead);
            while text.len() < size {
                let phrase: &[u8] = match rng.next() % 4 {
                    0 => b"the quick brown fox jumps over the lazy dog. ",
                    1 => b"pack my box with five dozen liquor jugs! ",
                    2 => b"0123456789abcdef",
                    _ => b"lorem ipsum dolor sit amet, consectetur ",
                };
                text.extend_from_slice(phrase);
            }
            text.truncate(size);
            text
        }
        "dm2ish" => {
            let mut out = Vec::with_capacity(size);
            let mut rng = Lcg(seed ^ 0xbeef);
            for i in 0..size {
                let (row, col) = (i / 257, i % 257);
                let noise = (rng.next() % 3) as u8;
                out.push(((row * 3 + col / 7) as u8).wrapping_add(noise));
            }
            out
        }
        _ => unreachable!(),
    }
}

/// (pattern, size, seed, encoded_len, fnv1a of encoded bytes) — captured
/// from the implementation immediately after native byte-parity validation.
/// Regenerate with `golden_generate` below if the values must change.
const GOLDEN: &[(&str, usize, u32, usize, u64)] = &[
    ("zeros", 0, 1, 12, 0x98d70bc3f52905d6),
    ("zeros", 5, 1, 17, 0x9c476a8b3480912d),
    ("zeros", 4096, 1, 147, 0x85e9908bac792547),
    ("cycle", 8192, 1, 500, 0xbb28a8de7efed83d),
    ("random", 65536, 7, 66288, 0x7399615a5bec590a),
    ("text", 40000, 3, 297, 0xd100e5209e7de872),
    ("text", 300000, 3, 449, 0x58e28e94c0c0f5f8),
    ("dm2ish", 100000, 9, 53814, 0x8c8f9a7a92d03e72),
];

#[test]
fn golden_streams() {
    for &(kind, size, seed, want_len, want_hash) in GOLDEN {
        let data = pattern(kind, size, seed);
        let budget = data.len() + data.len() / 8 + 256;
        let enc = encode_buffer(&data, budget).unwrap();
        assert_eq!(enc.len(), want_len, "{kind}/{size}: encoded length drifted");
        assert_eq!(fnv1a(&enc), want_hash, "{kind}/{size}: encoded bytes drifted");
        let dec = decode_buffer(&enc, data.len() + 32).unwrap();
        assert_eq!(dec, data, "{kind}/{size}: roundtrip");
    }
}

/// Regenerates the GOLDEN table: `cargo test golden_generate -- --ignored --nocapture`
#[test]
#[ignore]
fn golden_generate() {
    for &(kind, size, seed, _, _) in GOLDEN {
        let data = pattern(kind, size, seed);
        let budget = data.len() + data.len() / 8 + 256;
        let enc = encode_buffer(&data, budget).unwrap();
        println!(
            "    (\"{kind}\", {size}, {seed}, {}, {:#018x}),",
            enc.len(),
            fnv1a(&enc)
        );
    }
}

#[test]
fn roundtrip_all_paths() {
    // Covers: uncompressed (<8), bvxn (8..4095), bvx2 (>=4096), multi-block
    for &size in &[0usize, 1, 7, 8, 100, 4095, 4096, 8192, 40000, 262144, 1 << 20] {
        for kind in ["zeros", "cycle", "random", "text", "dm2ish"] {
            let data = pattern(kind, size, size as u32 ^ 0x55);
            let budget = data.len() + data.len() / 8 + 256;
            let enc = encode_buffer(&data, budget)
                .unwrap_or_else(|| panic!("{kind}/{size}: encode failed"));
            let dec = decode_buffer(&enc, data.len() + 32)
                .unwrap_or_else(|| panic!("{kind}/{size}: decode failed"));
            assert_eq!(dec, data, "{kind}/{size}");
        }
    }
}

#[test]
fn truncated_output_buffer() {
    let data = pattern("text", 10000, 1);
    let enc = encode_buffer(&data, 20000).unwrap();
    // DST-full: the reference API reports the full (undersized) buffer
    let dec = decode_buffer(&enc, 1000).unwrap();
    assert_eq!(dec.len(), 1000);
    assert_eq!(dec, data[..1000]);
}

#[test]
fn corrupt_streams_do_not_panic() {
    let mut rng = Lcg(0xfeed);
    for &size in &[100usize, 5000, 50000] {
        let data = pattern("dm2ish", size, size as u32);
        let enc = encode_buffer(&data, size * 2 + 256).unwrap();
        for _ in 0..200 {
            let mut bad = enc.clone();
            for _ in 0..1 + rng.next() % 6 {
                let pos = (rng.next() as usize) % bad.len();
                bad[pos] ^= rng.byte() | 1;
            }
            let _ = decode_buffer(&bad, size); // must not panic
        }
        for cut in (0..enc.len()).step_by((enc.len() / 29).max(1)) {
            let _ = decode_buffer(&enc[..cut], size);
        }
    }
}
