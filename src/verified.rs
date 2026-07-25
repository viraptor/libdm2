//! Formally specified core primitives, written in the Verus dialect.
//!
//! Everything inside the `verus! {}` block below compiles as ordinary Rust
//! under `cargo build` (ghost code — `spec`/`proof` items, `requires`/
//! `ensures` clauses, loop invariants — is erased), and is additionally
//! checkable by the [Verus](https://github.com/verus-lang/verus) SMT-based
//! verifier via `./verify.sh` (see `VERIFICATION.md`).
//!
//! The functions here are the *production* implementations for the pure
//! arithmetic core of the type-2 (Default) residual pipeline:
//!
//! - [`zigzag_encode`] / [`zigzag_decode`] — the zigzag mapping between
//!   signed residuals and unsigned code words, **proved** to be mutually
//!   inverse (via a 16-bit bit-vector proof).
//! - [`adjust_residual`] / [`unadjust_residual`] — deepmap2's
//!   decrement-negative-residuals quirk, **proved** mutually inverse for
//!   every representable input (`res > i16::MIN`).
//! - [`wrap_add_i16`] — 16-bit wrap-around addition expressed without
//!   `wrapping_add`, so its semantics are fully visible to the verifier.
//! - [`unpredict_none`] / [`unpredict_left`] / [`unpredict_up`] — verified
//!   row reconstruction for the three prediction modes the encoder emits,
//!   with elementwise functional postconditions. These are currently
//!   differential-tested against `predict::unpredict_row` (which writes
//!   into a caller buffer) rather than wired into the decode loop; see
//!   `VERIFICATION.md` for the plan to converge them.
//!
//! Every proved property is *also* enforced at `cargo test` time by
//! exhaustive or randomized tests in `tests/verified_props.rs`, so the
//! guarantees do not silently regress on machines without Verus.

use vstd::prelude::*;

verus! {

// ---------------------------------------------------------------------
// Zigzag coding
//
// The production formula (historically written over i32) is
//   encode(x) = ((x as i32 * 2) ^ ((x as i32) >> 15)) as u16
//   decode(z) = ((z >> 1) as i16) ^ (-((z & 1) as i16))
// Verus's bit-vector solver mode only reasons about unsigned integers, so
// the implementation below works on the unsigned 16-bit reinterpretation.
// `tests/verified_props.rs` checks equivalence with the historical signed
// formula exhaustively over all 65536 inputs.
// ---------------------------------------------------------------------

/// Spec: zigzag encoding on the unsigned 16-bit view.
/// `u & 0x8000 != 0` is exactly "the i16 reinterpretation is negative".
pub open spec fn spec_zigzag_encode(u: u16) -> u16 {
    (u << 1) ^ (if u & 0x8000 != 0 { 0xffffu16 } else { 0u16 })
}

/// Spec: zigzag decoding on the unsigned 16-bit view.
pub open spec fn spec_zigzag_decode(z: u16) -> u16 {
    (z >> 1) ^ (if z & 1 == 1 { 0xffffu16 } else { 0u16 })
}

/// Zigzag decode inverts zigzag encode, for all 2^16 inputs.
pub proof fn lemma_zigzag_roundtrip(u: u16)
    ensures
        spec_zigzag_decode(spec_zigzag_encode(u)) == u,
{
    // bit_vector mode cannot unfold spec functions, so restate the claim
    // with the definitions inlined; the surrounding (default) solver then
    // connects it to the spec functions definitionally.
    assert(((((u << 1) ^ (if u & 0x8000 != 0 { 0xffffu16 } else { 0u16 })) >> 1)
        ^ (if ((u << 1) ^ (if u & 0x8000 != 0 { 0xffffu16 } else { 0u16 })) & 1 == 1 {
            0xffffu16
        } else {
            0u16
        })) == u) by (bit_vector);
}

/// Zigzag encode is also a left inverse of decode: the mapping is a
/// bijection on the full 16-bit space, so every code word is meaningful.
pub proof fn lemma_zigzag_bijective(z: u16)
    ensures
        spec_zigzag_encode(spec_zigzag_decode(z)) == z,
{
    assert(((((z >> 1) ^ (if z & 1 == 1 { 0xffffu16 } else { 0u16 })) << 1)
        ^ (if ((z >> 1) ^ (if z & 1 == 1 { 0xffffu16 } else { 0u16 })) & 0x8000 != 0 {
            0xffffu16
        } else {
            0u16
        })) == z) by (bit_vector);
}

/// Map a signed residual to its unsigned zigzag code word.
/// 0 → 0, -1 → 1, 1 → 2, -2 → 3, 2 → 4, …
pub fn zigzag_encode(x: i16) -> (z: u16)
    ensures
        z == spec_zigzag_encode(x as u16),
{
    let u = x as u16;
    (u << 1) ^ (if u & 0x8000 != 0 { 0xffffu16 } else { 0u16 })
}

/// Map an unsigned zigzag code word back to its signed residual.
pub fn zigzag_decode(z: u16) -> (x: i16)
    ensures
        x as u16 == spec_zigzag_decode(z),
{
    ((z >> 1) ^ (if z & 1 == 1 { 0xffffu16 } else { 0u16 })) as i16
}

/// The signed-level statement: decoding an encoded residual gives it back.
pub proof fn lemma_zigzag_roundtrip_signed(x: i16)
    ensures
        spec_zigzag_decode(spec_zigzag_encode(x as u16)) as i16 == x,
{
    lemma_zigzag_roundtrip(x as u16);
}

// ---------------------------------------------------------------------
// Residual adjustment
//
// deepmap2's type-2 pipeline stores negative residuals decremented by one
// before zigzag coding (and the decoder increments negative values after
// zigzag decoding). The pair is inverse for every input the encoder can
// produce; `adjust_residual` needs `res > i16::MIN` because
// `i16::MIN - 1` is not representable (residuals of u8 pixel math are in
// [-511, 510], far inside that bound).
// ---------------------------------------------------------------------

pub open spec fn spec_adjust_residual(res: int) -> int {
    if res < 0 {
        res - 1
    } else {
        res
    }
}

pub open spec fn spec_unadjust_residual(v: int) -> int {
    if v < 0 {
        v + 1
    } else {
        v
    }
}

/// The adjustment pair is mutually inverse over the integers.
pub proof fn lemma_adjust_roundtrip(res: int)
    ensures
        spec_unadjust_residual(spec_adjust_residual(res)) == res,
{
}

/// Encoder side: decrement negative residuals (pre-zigzag).
pub fn adjust_residual(res: i16) -> (r: i16)
    requires
        res > i16::MIN,
    ensures
        r as int == spec_adjust_residual(res as int),
{
    if res < 0 {
        res - 1
    } else {
        res
    }
}

/// Decoder side: increment negative residuals (post-zigzag).
/// Total: safe on any input, including hostile streams — `v + 1` only
/// happens when `v < 0`, so it cannot overflow.
pub fn unadjust_residual(v: i16) -> (r: i16)
    ensures
        r as int == spec_unadjust_residual(v as int),
{
    if v < 0 {
        v + 1
    } else {
        v
    }
}

/// End-to-end residual coding: unadjust ∘ unzigzag ∘ zigzag ∘ adjust = id
/// for every encodable residual. This is the exact transformation a
/// type-2 residual undergoes between `encode_default_tile*` and
/// `decode_default_tile*` (the LZFSE layer in between is byte-transparent).
pub proof fn lemma_residual_pipeline_roundtrip(res: i16)
    requires
        res > i16::MIN,
    ensures
        spec_unadjust_residual(
            spec_zigzag_decode(
                spec_zigzag_encode((spec_adjust_residual(res as int)) as i16 as u16),
            ) as i16 as int,
        ) == res as int,
{
    let adj = spec_adjust_residual(res as int) as i16;
    lemma_zigzag_roundtrip_signed(adj);
    lemma_adjust_roundtrip(res as int);
}

// ---------------------------------------------------------------------
// 16-bit wrap-around addition
//
// `predict::unpredict_row` uses `i16::wrapping_add`; the version here is
// written with plain arithmetic and a mask so the verifier sees its full
// semantics without relying on standard-library specs. Equivalence with
// `wrapping_add` is checked in `tests/verified_props.rs`.
// ---------------------------------------------------------------------

/// Spec: addition modulo 2^16 on the unsigned view.
pub open spec fn spec_wrap_add(a: u16, b: u16) -> u16 {
    ((a + b) % 0x10000) as u16
}

/// Wrap-around i16 addition (two's complement), equivalent to
/// `a.wrapping_add(b)`.
pub fn wrap_add_i16(a: i16, b: i16) -> (r: i16)
    ensures
        r as u16 == spec_wrap_add(a as u16, b as u16),
{
    let au: u32 = (a as u16) as u32;
    let bu: u32 = (b as u16) as u32;
    let s: u32 = au + bu;
    assert(s & 0xffff == s % 0x10000) by (bit_vector);
    ((s & 0xffff) as u16) as i16
}

// ---------------------------------------------------------------------
// Row reconstruction (inverse prediction)
//
// Verified twins of the `None`/`Left`/`Up` arms of
// `predict::unpredict_row`, with functional postconditions. They return a
// fresh Vec rather than writing into a caller buffer because Verus's
// support for `&mut [T]` is still limited; the differential tests pin
// them to the production implementation.
// ---------------------------------------------------------------------

/// Mode 0 (None): residuals are the values.
pub fn unpredict_none(res: &[i16]) -> (out: Vec<i16>)
    ensures
        out.len() == res.len(),
        forall|i: int| 0 <= i < res@.len() ==> #[trigger] out@[i] == res@[i],
{
    let mut out: Vec<i16> = Vec::with_capacity(res.len());
    let mut k: usize = 0;
    while k < res.len()
        invariant
            k <= res.len(),
            out.len() == k,
            forall|i: int| 0 <= i < k ==> #[trigger] out@[i] == res@[i],
        decreases res.len() - k,
    {
        out.push(res[k]);
        k += 1;
    }
    out
}

/// Mode 2 (Left): each value is the residual plus the previous
/// reconstructed value in the same row (wrap-around), seeded with 0.
pub fn unpredict_left(res: &[i16]) -> (out: Vec<i16>)
    ensures
        out.len() == res.len(),
        forall|i: int|
            0 <= i < res@.len() ==> (#[trigger] out@[i]) as u16 == spec_wrap_add(
                res@[i] as u16,
                if i == 0 {
                    0u16
                } else {
                    out@[i - 1] as u16
                },
            ),
{
    let mut out: Vec<i16> = Vec::with_capacity(res.len());
    let mut k: usize = 0;
    while k < res.len()
        invariant
            k <= res.len(),
            out.len() == k,
            forall|i: int|
                0 <= i < k ==> (#[trigger] out@[i]) as u16 == spec_wrap_add(
                    res@[i] as u16,
                    if i == 0 {
                        0u16
                    } else {
                        out@[i - 1] as u16
                    },
                ),
        decreases res.len() - k,
    {
        let prev: i16 = if k == 0 {
            0
        } else {
            out[k - 1]
        };
        let v = wrap_add_i16(res[k], prev);
        out.push(v);
        k += 1;
    }
    out
}

/// Mode 3 (Up): each value is the residual plus the value directly above
/// (wrap-around).
pub fn unpredict_up(res: &[i16], prev: &[i16]) -> (out: Vec<i16>)
    requires
        res@.len() == prev@.len(),
    ensures
        out.len() == res.len(),
        forall|i: int|
            0 <= i < res@.len() ==> (#[trigger] out@[i]) as u16 == spec_wrap_add(
                res@[i] as u16,
                prev@[i] as u16,
            ),
{
    let mut out: Vec<i16> = Vec::with_capacity(res.len());
    let mut k: usize = 0;
    while k < res.len()
        invariant
            k <= res.len(),
            res@.len() == prev@.len(),
            out.len() == k,
            forall|i: int|
                0 <= i < k ==> (#[trigger] out@[i]) as u16 == spec_wrap_add(
                    res@[i] as u16,
                    prev@[i] as u16,
                ),
        decreases res.len() - k,
    {
        let v = wrap_add_i16(res[k], prev[k]);
        out.push(v);
        k += 1;
    }
    out
}

} // verus!
