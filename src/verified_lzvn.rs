//! Verus-verified LZVN decoder.
//!
//! This is the production decoder — `lzvn::decode` delegates here. LZVN
//! streams arrive from untrusted input (they are the entropy-coding layer
//! of every sub-4096-byte deepmap2 tile), which makes this the most
//! security-relevant parser in the crate. The verifier proves, for
//! **arbitrary** input bytes and output buffer:
//!
//! - no panic: every slice access is in bounds, no arithmetic over/underflow;
//! - termination: the source cursor strictly advances every iteration;
//! - the returned length never exceeds the output buffer's length, and
//!   the buffer's length is never changed.
//!
//! Functional correctness (that a valid stream decodes to the original
//! bytes) is deliberately *not* claimed here — it is enforced by the
//! encoder round-trip and opcode tests in `tests/` plus the differential
//! fuzz suite in `tests/verified_props.rs`.
//!
//! The opcode layout matches Apple's reference implementation
//! (lzvn_decode_base.c); see `deepmap2.md` for the stream-level rules
//! (8-byte EOS marker, truncate-on-full-output behavior).

use vstd::prelude::*;

verus! {

// Opcode classes, indexed by the first opcode byte:
// 0=SmlD, 1=MedD, 2=SmlM, 3=LrgD, 4=PreD, 5=LrgM, 6=SmlL, 7=LrgL,
// 8=Eos, 9=Nop, 10=Undef — matching Apple's 256-entry dispatch table.
#[rustfmt::skip]
pub const OP_TABLE: [u8; 256] = [
    0,0,0,0,0,0,8,3, 0,0,0,0,0,0,9,3, // 0x00
    0,0,0,0,0,0,9,3, 0,0,0,0,0,0,10,3, // 0x10
    0,0,0,0,0,0,10,3, 0,0,0,0,0,0,10,3, // 0x20
    0,0,0,0,0,0,10,3, 0,0,0,0,0,0,10,3, // 0x30
    0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x40
    0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x50
    0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x60
    10,10,10,10,10,10,10,10, 10,10,10,10,10,10,10,10, // 0x70
    0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x80
    0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x90
    1,1,1,1,1,1,1,1, 1,1,1,1,1,1,1,1, // 0xA0
    1,1,1,1,1,1,1,1, 1,1,1,1,1,1,1,1, // 0xB0
    0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0xC0
    10,10,10,10,10,10,10,10, 10,10,10,10,10,10,10,10, // 0xD0
    7,6,6,6,6,6,6,6, 6,6,6,6,6,6,6,6, // 0xE0
    5,2,2,2,2,2,2,2, 2,2,2,2,2,2,2,2, // 0xF0
];

/// Copy `l` literal bytes from `src[sp..]` to `dst[dp..]`, truncating to
/// the remaining output space (Apple's decoder stops when the output is
/// full). Fails only when the *un*-truncated run extends past the
/// source. Returns the advanced `(sp, dp)` cursors on success.
fn copy_literal(src: &[u8], sp: usize, dst: &mut [u8], dp: usize, l: usize) -> (r: Option<
    (usize, usize)>)
    requires
        sp <= src.len(),
        dp <= old(dst).len(),
    ensures
        final(dst)@.len() == old(dst)@.len(),
        match r {
            Some(c) => sp <= c.0 <= src.len() && c.1 <= final(dst)@.len(),
            None => true,
        },
{
    if l > src.len() - sp {
        return None;
    }
    let avail = dst.len() - dp;
    let l_eff = if l < avail {
        l
    } else {
        avail
    };
    let ghost n0 = dst@.len();
    let mut i: usize = 0;
    while i < l_eff
        invariant
            dst@.len() <= usize::MAX,
            dst@.len() == n0,
            i <= l_eff,
            sp + l_eff <= src.len(),
            dp + l_eff <= dst@.len(),
        decreases l_eff - i,
    {
        dst[dp + i] = src[sp + i];
        i += 1;
    }
    Some((sp + l_eff, dp + l_eff))
}

/// Copy an `m`-byte match from distance `d` back, truncating to the
/// remaining output space. Byte-by-byte to give overlapping matches
/// (e.g. d=1 RLE) the correct semantics. Fails on d == 0 or a distance
/// reaching before the start of the output. Returns the advanced output
/// cursor on success.
fn copy_match(dst: &mut [u8], dp: usize, d: usize, m: usize) -> (r: Option<usize>)
    requires
        dp <= old(dst).len(),
    ensures
        final(dst)@.len() == old(dst)@.len(),
        match r {
            Some(c) => c <= final(dst)@.len(),
            None => true,
        },
{
    if d == 0 || d > dp {
        return None;
    }
    let avail = dst.len() - dp;
    let m_eff = if m < avail {
        m
    } else {
        avail
    };
    let ghost n0 = dst@.len();
    let mut i: usize = 0;
    while i < m_eff
        invariant
            dst@.len() <= usize::MAX,
            dst@.len() == n0,
            i <= m_eff,
            1 <= d <= dp,
            dp + m_eff <= dst@.len(),
        decreases m_eff - i,
    {
        dst[dp + i] = dst[dp + i - d];
        i += 1;
    }
    Some(dp + m_eff)
}

/// Decode an LZVN stream into `dst`. Returns the number of bytes
/// produced, or `None` for a malformed stream. Verified panic-free and
/// terminating for arbitrary `src`/`dst`.
pub fn decode(src: &[u8], dst: &mut [u8]) -> (result: Option<usize>)
    ensures
        final(dst)@.len() == old(dst)@.len(),
        match result {
            Some(n) => n <= final(dst)@.len(),
            None => true,
        },
{
    let _n0: usize = dst.len();
    let mut sp: usize = 0; // source position
    let mut dp: usize = 0; // destination position
    let mut d_prev: usize = 0; // previous match distance

    while sp < src.len()
        invariant
            _n0 == old(dst)@.len(),
            dst@.len() == _n0,
            sp <= src.len(),
            dp <= dst@.len(),
        decreases src.len() - sp,
    {
        // Output buffer full — stop decoding (Apple's decoder does the same).
        if dp >= dst.len() {
            return Some(dp);
        }
        let opc = src[sp];

        match OP_TABLE[opc as usize] {
            8u8 => {
                // Eos
                return Some(dp);
            }
            9u8 => {
                // Nop
                sp += 1;
            }
            0u8 => {
                // SmlD: LLMMMDDD + 1 distance byte
                let l = ((opc >> 6) & 3) as usize;
                let m = (((opc >> 3) & 7) as usize) + 3;
                if src.len() - sp <= 2 + l {
                    return None;
                }
                let d = (((opc & 7) as usize) << 8) | (src[sp + 1] as usize);
                sp += 2;
                match copy_literal(src, sp, dst, dp, l) {
                    Some(c) => {
                        sp = c.0;
                        dp = c.1;
                    }
                    None => {
                        return None;
                    }
                }
                match copy_match(dst, dp, d, m) {
                    Some(c) => {
                        dp = c;
                    }
                    None => {
                        return None;
                    }
                }
                d_prev = d;
            }
            1u8 => {
                // MedD: 101LLMMM + 2 bytes of DDDDDDDD DDDDDDMM
                let l = ((opc >> 3) & 3) as usize;
                if src.len() - sp <= 3 + l {
                    return None;
                }
                let opc23 = (src[sp + 1] as u16) | ((src[sp + 2] as u16) << 8);
                let m = ((((opc & 7) << 2) | ((opc23 & 3) as u8)) as usize) + 3;
                let d = (opc23 >> 2) as usize;
                sp += 3;
                match copy_literal(src, sp, dst, dp, l) {
                    Some(c) => {
                        sp = c.0;
                        dp = c.1;
                    }
                    None => {
                        return None;
                    }
                }
                match copy_match(dst, dp, d, m) {
                    Some(c) => {
                        dp = c;
                    }
                    None => {
                        return None;
                    }
                }
                d_prev = d;
            }
            3u8 => {
                // LrgD: LLMMM111 + 2 distance bytes
                let l = ((opc >> 6) & 3) as usize;
                let m = (((opc >> 3) & 7) as usize) + 3;
                if src.len() - sp <= 3 + l {
                    return None;
                }
                let d = (src[sp + 1] as usize) | ((src[sp + 2] as usize) << 8);
                sp += 3;
                match copy_literal(src, sp, dst, dp, l) {
                    Some(c) => {
                        sp = c.0;
                        dp = c.1;
                    }
                    None => {
                        return None;
                    }
                }
                match copy_match(dst, dp, d, m) {
                    Some(c) => {
                        dp = c;
                    }
                    None => {
                        return None;
                    }
                }
                d_prev = d;
            }
            4u8 => {
                // PreD: LLMMM110, reuses previous distance
                let l = ((opc >> 6) & 3) as usize;
                let m = (((opc >> 3) & 7) as usize) + 3;
                if src.len() - sp <= 1 + l {
                    return None;
                }
                sp += 1;
                match copy_literal(src, sp, dst, dp, l) {
                    Some(c) => {
                        sp = c.0;
                        dp = c.1;
                    }
                    None => {
                        return None;
                    }
                }
                match copy_match(dst, dp, d_prev, m) {
                    Some(c) => {
                        dp = c;
                    }
                    None => {
                        return None;
                    }
                }
            }
            2u8 => {
                // SmlM: 1111MMMM, previous distance
                let m = (opc & 0xf) as usize;
                if src.len() - sp <= 1 {
                    return None;
                }
                sp += 1;
                match copy_match(dst, dp, d_prev, m) {
                    Some(c) => {
                        dp = c;
                    }
                    None => {
                        return None;
                    }
                }
            }
            5u8 => {
                // LrgM: 0xF0 + length byte, previous distance
                if src.len() - sp <= 2 {
                    return None;
                }
                let m = (src[sp + 1] as usize) + 16;
                sp += 2;
                match copy_match(dst, dp, d_prev, m) {
                    Some(c) => {
                        dp = c;
                    }
                    None => {
                        return None;
                    }
                }
            }
            6u8 => {
                // SmlL: 1110LLLL literal run
                let l = (opc & 0xf) as usize;
                if src.len() - sp <= 1 + l {
                    return None;
                }
                sp += 1;
                match copy_literal(src, sp, dst, dp, l) {
                    Some(c) => {
                        sp = c.0;
                        dp = c.1;
                    }
                    None => {
                        return None;
                    }
                }
            }
            7u8 => {
                // LrgL: 0xE0 + length byte
                if src.len() - sp <= 2 {
                    return None;
                }
                let l = (src[sp + 1] as usize) + 16;
                if src.len() - sp <= 2 + l {
                    return None;
                }
                sp += 2;
                match copy_literal(src, sp, dst, dp, l) {
                    Some(c) => {
                        sp = c.0;
                        dp = c.1;
                    }
                    None => {
                        return None;
                    }
                }
            }
            _ => {
                // Undef
                return None;
            }
        }
    }
    // Ran off the end of the source without an EOS marker.
    None
}

} // verus!
