//! LZFSE container encoder/decoder — a Rust port of Apple's lzfse
//! (lzfse_encode_base.c, lzfse_decode_base.c, lzfse_fse.c).
//!
//! `decode_buffer` accepts exactly the streams `lzfse_decode_buffer` accepts
//! (all block types: bvx-, bvx1, bvx2, bvxn), with identical output including
//! truncation and error behavior.
//!
//! `encode_buffer` follows the variant of the algorithm shipped in
//! `libcompression.dylib` — the one vImage actually calls — which differs
//! from the published sources in its literal-lag flush threshold and its
//! literal-capacity accounting when splitting a match (see deepmap2.md).
//! That is what makes whole deepmap2 streams byte-identical to
//! `vImageDeepmap2Encode`; it also means output can differ from a build of
//! the open-source release on inputs with long literal runs.
//!
//! One further divergence: for inputs below the 4096-byte LZVN threshold the
//! reference embeds an lzvn-compressed (bvxn) block, produced by its own lzvn
//! match finder; we use our lzvn encoder (see lzvn.rs), which emits a valid
//! but not byte-identical payload. deepmap2 never takes that path — tiles
//! below the threshold use raw LZVN with no container.

mod decode;
mod encode;
mod fse;

pub use decode::decode_buffer;
pub use encode::encode_buffer;

use crate::error::{Dm2Error, Result};
use crate::lzvn;

// ------------------------------------------------------------------
//  Format constants (lzfse_internal.h)
// ------------------------------------------------------------------

pub(crate) const LZFSE_ENCODE_L_SYMBOLS: usize = 20;
pub(crate) const LZFSE_ENCODE_M_SYMBOLS: usize = 20;
pub(crate) const LZFSE_ENCODE_D_SYMBOLS: usize = 64;
pub(crate) const LZFSE_ENCODE_LITERAL_SYMBOLS: usize = 256;
pub(crate) const LZFSE_ENCODE_L_STATES: i32 = 64;
pub(crate) const LZFSE_ENCODE_M_STATES: i32 = 64;
pub(crate) const LZFSE_ENCODE_D_STATES: i32 = 256;
pub(crate) const LZFSE_ENCODE_LITERAL_STATES: i32 = 1024;
pub(crate) const LZFSE_MATCHES_PER_BLOCK: usize = 10000;
pub(crate) const LZFSE_LITERALS_PER_BLOCK: usize = 4 * LZFSE_MATCHES_PER_BLOCK;

pub(crate) const LZFSE_ENDOFSTREAM_BLOCK_MAGIC: u32 = 0x24787662; // bvx$
pub(crate) const LZFSE_UNCOMPRESSED_BLOCK_MAGIC: u32 = 0x2d787662; // bvx-
pub(crate) const LZFSE_COMPRESSEDV1_BLOCK_MAGIC: u32 = 0x31787662; // bvx1
pub(crate) const LZFSE_COMPRESSEDV2_BLOCK_MAGIC: u32 = 0x32787662; // bvx2
pub(crate) const LZFSE_COMPRESSEDLZVN_BLOCK_MAGIC: u32 = 0x6e787662; // bvxn

pub(crate) const LZFSE_ENCODE_MAX_L_VALUE: u32 = 315;
pub(crate) const LZFSE_ENCODE_MAX_M_VALUE: u32 = 2359;
pub(crate) const LZFSE_ENCODE_MAX_D_VALUE: i64 = 262139;

#[rustfmt::skip]
pub(crate) const L_EXTRA_BITS: [u8; LZFSE_ENCODE_L_SYMBOLS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 5, 8,
];
#[rustfmt::skip]
pub(crate) const L_BASE_VALUE: [i32; LZFSE_ENCODE_L_SYMBOLS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 20, 28, 60,
];
#[rustfmt::skip]
pub(crate) const M_EXTRA_BITS: [u8; LZFSE_ENCODE_M_SYMBOLS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 5, 8, 11,
];
#[rustfmt::skip]
pub(crate) const M_BASE_VALUE: [i32; LZFSE_ENCODE_M_SYMBOLS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 24, 56, 312,
];
#[rustfmt::skip]
pub(crate) const D_EXTRA_BITS: [u8; LZFSE_ENCODE_D_SYMBOLS] = [
    0,  0,  0,  0,  1,  1,  1,  1,  2,  2,  2,  2,  3,  3,  3,  3,
    4,  4,  4,  4,  5,  5,  5,  5,  6,  6,  6,  6,  7,  7,  7,  7,
    8,  8,  8,  8,  9,  9,  9,  9,  10, 10, 10, 10, 11, 11, 11, 11,
    12, 12, 12, 12, 13, 13, 13, 13, 14, 14, 14, 14, 15, 15, 15, 15,
];
#[rustfmt::skip]
pub(crate) const D_BASE_VALUE: [i32; LZFSE_ENCODE_D_SYMBOLS] = [
    0,      1,      2,      3,     4,     6,     8,     10,    12,    16,
    20,     24,     28,     36,    44,    52,    60,    76,    92,    108,
    124,    156,    188,    220,   252,   316,   380,   444,   508,   636,
    764,    892,    1020,   1276,  1532,  1788,  2044,  2556,  3068,  3580,
    4092,   5116,   6140,   7164,  8188,  10236, 12284, 14332, 16380, 20476,
    24572,  28668,  32764,  40956, 49148, 57340, 65532, 81916, 98300, 114684,
    131068, 163836, 196604, 229372,
];

/// V2 header: magic + n_raw_bytes + 3 packed u64 fields, then the compressed
/// freq tables.
pub(crate) const V2_HEADER_FIXED_SIZE: usize = 32;
/// sizeof(lzfse_compressed_block_header_v2): fixed part + worst-case freq.
pub(crate) const V2_HEADER_FULL_SIZE: usize = V2_HEADER_FIXED_SIZE
    + 2 * (LZFSE_ENCODE_L_SYMBOLS
        + LZFSE_ENCODE_M_SYMBOLS
        + LZFSE_ENCODE_D_SYMBOLS
        + LZFSE_ENCODE_LITERAL_SYMBOLS);
/// sizeof(lzfse_compressed_block_header_v1), including trailing padding.
pub(crate) const V1_HEADER_SIZE: usize = 772;

#[inline]
pub(crate) fn load4(src: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(src[pos..pos + 4].try_into().unwrap())
}

#[inline]
pub(crate) fn load8(src: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(src[pos..pos + 8].try_into().unwrap())
}

// ------------------------------------------------------------------
//  deepmap2-facing API (unchanged semantics)
// ------------------------------------------------------------------

/// Apple's deepmap2 decoder only accepts two LZFSE stream forms: raw LZVN
/// (no container header) for tiles below 4096 raw bytes, and `bvx2` blocks
/// for larger tiles. `lzfse_encode_buffer` would emit `bvxn`
/// (LZVN-in-container) blocks for medium-sized inputs, which deepmap2
/// rejects, so we use raw LZVN up to the cutoff and the container encoder
/// above it, where it reliably picks `bvx2`.
const LZVN_THRESHOLD: usize = 4096;

const BVX2_MAGIC: [u8; 4] = [0x62, 0x76, 0x78, 0x32];

pub fn compress(src: &[u8]) -> Result<Vec<u8>> {
    if src.len() < LZVN_THRESHOLD {
        let encoded = lzvn::encode(src);
        if encoded.is_empty() {
            return Err(Dm2Error::EncodeFailed);
        }
        return Ok(encoded);
    }

    let max_out = src.len() + (src.len() / 8) + 256;
    encode_buffer(src, max_out).ok_or(Dm2Error::EncodeFailed)
}

pub fn decompress(src: &[u8], max_output: usize) -> Result<Vec<u8>> {
    if src.is_empty() {
        return Ok(Vec::new());
    }

    // Detect format: bvx2 header means LZFSE container, otherwise raw LZVN
    let is_lzfse_container = src.len() >= 4 && src[0..4] == BVX2_MAGIC;

    if is_lzfse_container {
        let dst = decode_buffer(src, max_output).ok_or(Dm2Error::DecodeFailed)?;
        if dst.is_empty() {
            return Err(Dm2Error::DecodeFailed);
        }
        Ok(dst)
    } else {
        // Raw LZVN
        let mut dst = vec![0u8; max_output];
        let n = lzvn::decode(src, &mut dst).ok_or(Dm2Error::DecodeFailed)?;
        dst.truncate(n);
        Ok(dst)
    }
}
