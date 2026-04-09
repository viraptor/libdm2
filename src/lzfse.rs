use crate::error::{Dm2Error, Result};
use crate::lzvn;

extern "C" {
    fn lzfse_encode_scratch_size() -> usize;
    fn lzfse_decode_scratch_size() -> usize;
    fn lzfse_encode_buffer(
        dst: *mut u8, dst_size: usize,
        src: *const u8, src_size: usize,
        scratch: *mut u8,
    ) -> usize;
    fn lzfse_decode_buffer(
        dst: *mut u8, dst_size: usize,
        src: *const u8, src_size: usize,
        scratch: *mut u8,
    ) -> usize;
}

/// Apple's deepmap2 decoder only accepts two LZFSE stream forms: raw LZVN
/// (no container header) for tiles below 4096 raw bytes, and `bvx2` blocks
/// for larger tiles. The open-source
/// `lzfse_encode_buffer` would emit `bvxn` (LZVN-in-container) blocks for
/// medium-sized inputs, which deepmap2 rejects, so we use our native LZVN
/// encoder up to the cutoff and only call into the C library above it,
/// where it reliably picks `bvx2`.
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

    let scratch_size = unsafe { lzfse_encode_scratch_size() };
    let mut scratch = vec![0u8; scratch_size];
    let max_out = src.len() + (src.len() / 8) + 256;
    let mut dst = vec![0u8; max_out];

    let written = unsafe {
        lzfse_encode_buffer(
            dst.as_mut_ptr(), dst.len(),
            src.as_ptr(), src.len(),
            scratch.as_mut_ptr(),
        )
    };
    if written == 0 {
        return Err(Dm2Error::EncodeFailed);
    }
    dst.truncate(written);
    Ok(dst)
}

pub fn decompress(src: &[u8], max_output: usize) -> Result<Vec<u8>> {
    if src.is_empty() {
        return Ok(Vec::new());
    }

    // Detect format: bvx2 header means LZFSE container, otherwise raw LZVN
    let is_lzfse_container = src.len() >= 4 && src[0..4] == BVX2_MAGIC;

    if is_lzfse_container {
        let scratch_size = unsafe { lzfse_decode_scratch_size() };
        let mut scratch = vec![0u8; scratch_size];
        let mut dst = vec![0u8; max_output];

        let written = unsafe {
            lzfse_decode_buffer(
                dst.as_mut_ptr(), dst.len(),
                src.as_ptr(), src.len(),
                scratch.as_mut_ptr(),
            )
        };
        if written == 0 {
            return Err(Dm2Error::DecodeFailed);
        }
        dst.truncate(written);
        Ok(dst)
    } else {
        // Raw LZVN
        let mut dst = vec![0u8; max_output];
        let n = lzvn::decode(src, &mut dst).ok_or(Dm2Error::DecodeFailed)?;
        dst.truncate(n);
        Ok(dst)
    }
}
