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
    let u: u16 = (z >> 1) ^ (if z & 1 == 1 { 0xffffu16 } else { 0u16 });
    proof {
        // Truncating-cast semantics are only exported to the bit-vector
        // solver; hand the default solver the u16→i16→u16 roundtrip fact.
        assert(((u as i16) as u16) == u) by (bit_vector);
    }
    u as i16
}

/// The signed-level statement: decoding an encoded residual gives it back.
pub proof fn lemma_zigzag_roundtrip_signed(x: i16)
    ensures
        spec_zigzag_decode(spec_zigzag_encode(x as u16)) as i16 == x,
{
    lemma_zigzag_roundtrip(x as u16);
    assert(((x as u16) as i16) == x) by (bit_vector);
}

/// The implementation matches the arithmetic characterization documented
/// in deepmap2.md: `zigzag_encode(x) = x >= 0 ? 2*x : -2*x - 1`.
pub proof fn lemma_zigzag_encode_matches_documented(x: i16)
    ensures
        spec_zigzag_encode(x as u16) as int == if x >= 0 {
            2 * (x as int)
        } else {
            -2 * (x as int) - 1
        },
{
    assert((((x as u16) << 1) ^ (if (x as u16) & 0x8000 != 0 { 0xffffu16 } else { 0u16 })) as int
        == if x >= 0 {
            2 * (x as int)
        } else {
            -2 * (x as int) - 1
        }) by (bit_vector);
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
//
// These use `#[verifier::allow_in_spec]` + `returns` instead of separate
// twin spec functions: one definition serves as both the executable code
// and its spec-mode meaning, so the lemmas below can name the functions
// directly.
// ---------------------------------------------------------------------

/// Encoder side: decrement negative residuals (pre-zigzag).
#[verifier::allow_in_spec]
pub fn adjust_residual(res: i16) -> (r: i16)
    requires
        res > i16::MIN,
    returns
        (if res < 0 {
            (res - 1) as i16
        } else {
            res
        }),
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
#[verifier::allow_in_spec]
pub fn unadjust_residual(v: i16) -> (r: i16)
    returns
        (if v < 0 {
            (v + 1) as i16
        } else {
            v
        }),
{
    if v < 0 {
        v + 1
    } else {
        v
    }
}

/// The adjustment pair is mutually inverse for every encodable residual.
pub proof fn lemma_adjust_roundtrip(res: i16)
    requires
        res > i16::MIN,
    ensures
        unadjust_residual(adjust_residual(res)) == res,
{
}

/// End-to-end residual coding: unadjust ∘ unzigzag ∘ zigzag ∘ adjust = id
/// for every encodable residual. This is the exact transformation a
/// type-2 residual undergoes between `encode_default_tile*` and
/// `decode_default_tile*` (the LZFSE layer in between is byte-transparent).
pub proof fn lemma_residual_pipeline_roundtrip(res: i16)
    requires
        res > i16::MIN,
    ensures
        unadjust_residual(
            spec_zigzag_decode(spec_zigzag_encode(adjust_residual(res) as u16)) as i16,
        ) == res,
{
    lemma_zigzag_roundtrip_signed(adjust_residual(res));
}

// ---------------------------------------------------------------------
// 16-bit wrap-around addition
//
// vstd ships trusted specifications for the primitive wrapping ops
// (`vstd::wrapping::i16_specs::wrapping_add` is an int-level, in-range
// formulation the default solver reasons about directly), so the verified
// wrapper simply names that semantics in its postcondition.
// ---------------------------------------------------------------------

/// Spec: two's-complement wrap-around addition on i16, re-exported from
/// vstd's trusted spec so callers here have a stable local name.
pub open spec fn spec_wrap_add(a: i16, b: i16) -> i16 {
    vstd::wrapping::i16_specs::wrapping_add(a, b)
}

/// Wrap-around i16 addition. The decoder uses this for prediction sums so
/// hostile residuals cannot overflow-panic debug builds.
pub fn wrap_add_i16(a: i16, b: i16) -> (r: i16)
    ensures
        r == spec_wrap_add(a, b),
{
    a.wrapping_add(b)
}

// ---------------------------------------------------------------------
// Row reconstruction (inverse prediction)
//
// Verified implementations of all five prediction-mode inversions from
// deepmap2.md, writing into the caller's buffer exactly like the
// production call sites need. `predict::unpredict_row` dispatches to
// these, so the decode path runs this code.
// ---------------------------------------------------------------------

/// Mode 0 (None): residuals are the values.
pub fn unpredict_none(res: &[i16], out: &mut [i16])
    requires
        old(out).len() == res.len(),
    ensures
        final(out)@.len() == res@.len(),
        forall|i: int| 0 <= i < res@.len() ==> #[trigger] final(out)@[i] == res@[i],
{
    let mut k: usize = 0;
    while k < res.len()
        invariant
            out@.len() == res@.len(),
            k <= res.len(),
            forall|i: int| 0 <= i < k ==> #[trigger] out@[i] == res@[i],
        decreases res.len() - k,
    {
        out[k] = res[k];
        k += 1;
    }
}

/// Mode 2 (Left): each value is the residual plus the previous
/// reconstructed value in the same row (wrap-around), seeded with 0.
pub fn unpredict_left(res: &[i16], out: &mut [i16])
    requires
        old(out).len() == res.len(),
    ensures
        final(out)@.len() == res@.len(),
        forall|i: int|
            0 <= i < res@.len() ==> (#[trigger] final(out)@[i]) == spec_wrap_add(
                res@[i],
                if i == 0 {
                    0i16
                } else {
                    final(out)@[i - 1]
                },
            ),
{
    let mut k: usize = 0;
    while k < res.len()
        invariant
            out@.len() == res@.len(),
            k <= res.len(),
            forall|i: int|
                0 <= i < k ==> (#[trigger] out@[i]) == spec_wrap_add(
                    res@[i],
                    if i == 0 {
                        0i16
                    } else {
                        out@[i - 1]
                    },
                ),
        decreases res.len() - k,
    {
        let prev: i16 = if k == 0 {
            0
        } else {
            out[k - 1]
        };
        out[k] = wrap_add_i16(res[k], prev);
        k += 1;
    }
}

/// Mode 3 (Up): each value is the residual plus the value directly above
/// (wrap-around).
pub fn unpredict_up(res: &[i16], prev: &[i16], out: &mut [i16])
    requires
        res@.len() == prev@.len(),
        old(out).len() == res.len(),
    ensures
        final(out)@.len() == res@.len(),
        forall|i: int|
            0 <= i < res@.len() ==> (#[trigger] final(out)@[i]) == spec_wrap_add(
                res@[i],
                prev@[i],
            ),
{
    let mut k: usize = 0;
    while k < res.len()
        invariant
            out@.len() == res@.len(),
            res@.len() == prev@.len(),
            k <= res.len(),
            forall|i: int|
                0 <= i < k ==> (#[trigger] out@[i]) == spec_wrap_add(res@[i], prev@[i]),
        decreases res.len() - k,
    {
        out[k] = wrap_add_i16(res[k], prev[k]);
        k += 1;
    }
}

/// Spec: |x| over int.
pub open spec fn spec_abs(x: int) -> int {
    if x < 0 {
        -x
    } else {
        x
    }
}

/// Spec: the 2-way Paeth predictor selection (deepmap2.md mode 1).
/// With p = up + left - upleft: |p - left| = |up - upleft| and
/// |p - up| = |left - upleft|, so the selection reduces to comparing the
/// neighbors' distances from the corner. Prefers left on ties.
pub open spec fn spec_paeth2(left: i16, up: i16, upleft: i16) -> i16 {
    if spec_abs(left - upleft) < spec_abs(up - upleft) {
        up
    } else {
        left
    }
}

/// Mode 1 (UpLeft): 2-way Paeth. Position 0 falls back to Up.
pub fn unpredict_upleft(res: &[i16], prev: &[i16], out: &mut [i16])
    requires
        res@.len() == prev@.len(),
        old(out).len() == res.len(),
    ensures
        final(out)@.len() == res@.len(),
        forall|i: int|
            0 <= i < res@.len() ==> (#[trigger] final(out)@[i]) == spec_wrap_add(
                res@[i],
                if i == 0 {
                    prev@[0]
                } else {
                    spec_paeth2(final(out)@[i - 1], prev@[i], prev@[i - 1])
                },
            ),
{
    let mut k: usize = 0;
    while k < res.len()
        invariant
            out@.len() == res@.len(),
            res@.len() == prev@.len(),
            k <= res.len(),
            forall|i: int|
                0 <= i < k ==> (#[trigger] out@[i]) == spec_wrap_add(
                    res@[i],
                    if i == 0 {
                        prev@[0]
                    } else {
                        spec_paeth2(out@[i - 1], prev@[i], prev@[i - 1])
                    },
                ),
        decreases res.len() - k,
    {
        let pred: i16 = if k == 0 {
            prev[0]
        } else {
            // p = up + left - upleft; pa = |p - left|; pb = |p - up|.
            // Computed in i32 (cannot overflow for i16 inputs).
            let p: i32 = prev[k] as i32 + out[k - 1] as i32 - prev[k - 1] as i32;
            let da: i32 = p - out[k - 1] as i32;
            let db: i32 = p - prev[k] as i32;
            let pa: i32 = if da < 0 { -da } else { da };
            let pb: i32 = if db < 0 { -db } else { db };
            if pb < pa {
                prev[k]
            } else {
                out[k - 1]
            }
        };
        out[k] = wrap_add_i16(res[k], pred);
        k += 1;
    }
}

/// Spec: the Mean predictor (deepmap2.md mode 4):
/// floor((left + up + 1 + [sum negative]) / 2) — i.e. `sum >> 1` after the
/// truncation-toward-zero correction. Spec `/` on int is Euclidean, which
/// equals floor for the positive divisor 2.
pub open spec fn spec_mean_pred(left: i16, up: i16) -> int {
    let s0 = left + up + 1;
    let s1: int = if s0 < 0 {
        s0 + 1
    } else {
        s0
    };
    s1 / 2
}

/// Mode 4 (Mean): average of left and up neighbors. Position 0 falls back
/// to Up.
pub fn unpredict_mean(res: &[i16], prev: &[i16], out: &mut [i16])
    requires
        res@.len() == prev@.len(),
        old(out).len() == res.len(),
    ensures
        final(out)@.len() == res@.len(),
        forall|i: int|
            0 <= i < res@.len() ==> (#[trigger] final(out)@[i]) == spec_wrap_add(
                res@[i],
                if i == 0 {
                    prev@[0]
                } else {
                    spec_mean_pred(final(out)@[i - 1], prev@[i]) as i16
                },
            ),
{
    let mut k: usize = 0;
    while k < res.len()
        invariant
            out@.len() == res@.len(),
            res@.len() == prev@.len(),
            k <= res.len(),
            forall|i: int|
                0 <= i < k ==> (#[trigger] out@[i]) == spec_wrap_add(
                    res@[i],
                    if i == 0 {
                        prev@[0]
                    } else {
                        spec_mean_pred(out@[i - 1], prev@[i]) as i16
                    },
                ),
        decreases res.len() - k,
    {
        let pred: i16 = if k == 0 {
            prev[0]
        } else {
            // sum = left + up + 1; negative sums get the truncation
            // correction; then floor-halve. Production uses `sum >> 1`;
            // `(s-1)/2` is the same floor for negative s, written with
            // division so the semantics are visible to the verifier.
            let s0: i32 = out[k - 1] as i32 + prev[k] as i32 + 1;
            let s1: i32 = if s0 < 0 { s0 + 1 } else { s0 };
            let half: i32 = if s1 >= 0 { s1 / 2 } else { (s1 - 1) / 2 };
            half as i16
        };
        out[k] = wrap_add_i16(res[k], pred);
        k += 1;
    }
}

// ---------------------------------------------------------------------
// YCoCg color transform (deepmap2.md "Color transform")
//
//   Forward:  Co = R - B;  t = B + Co/2;  Cg = G - t;  Y = t + Cg/2
//   Inverse:  t = Y - Cg/2;  G = Cg + t;  B = t - Co/2;  R = Co + B
//
// with truncation-toward-zero halving. The round-trip proof needs no
// reasoning about the division at all: Co and Cg pass through unchanged,
// and t (hence Y) is derived from the *same* halved terms in both
// directions, so everything cancels linearly.
// ---------------------------------------------------------------------

/// Spec: truncation-toward-zero halving (matches Rust's `x / 2`).
pub open spec fn spec_div2_trunc(x: int) -> int {
    if x >= 0 {
        x / 2
    } else {
        -((-x) / 2)
    }
}

/// Truncating halve, as used by the deepmap2 color transform.
pub fn div2_trunc(x: i16) -> (r: i16)
    requires
        x > i16::MIN,
    ensures
        r as int == spec_div2_trunc(x as int),
{
    x / 2
}

/// Spec: forward transform of one pixel (channels as ints).
pub open spec fn spec_ycocg_forward(r: int, g: int, b: int) -> (int, int, int) {
    let co = r - b;
    let t = b + spec_div2_trunc(co);
    let cg = g - t;
    let y = t + spec_div2_trunc(cg);
    (y, co, cg)
}

/// Spec: inverse transform of one pixel.
pub open spec fn spec_ycocg_inverse(y: int, co: int, cg: int) -> (int, int, int) {
    let t = y - spec_div2_trunc(cg);
    let g = cg + t;
    let b = t - spec_div2_trunc(co);
    let r = co + b;
    (r, g, b)
}

/// The transform is exactly invertible — not just for 8-bit inputs but
/// for any integers: Co/Cg are passed through and the halved terms cancel.
pub proof fn lemma_ycocg_roundtrip(r: int, g: int, b: int)
    ensures
        spec_ycocg_inverse(
            spec_ycocg_forward(r, g, b).0,
            spec_ycocg_forward(r, g, b).1,
            spec_ycocg_forward(r, g, b).2,
        ) == (r, g, b),
{
}

/// Forward YCoCg for one 8-bit pixel, as used by the type-2 encoder.
/// The outputs are small (|Co| ≤ 255, |Cg| ≤ 382, −318 ≤ Y ≤ 573), far
/// inside i16, so the residual pipeline's `res > i16::MIN` precondition
/// always holds downstream.
pub fn ycocg_forward_pixel(r: u8, g: u8, b: u8) -> (out: (i16, i16, i16))
    ensures
        (out.0 as int, out.1 as int, out.2 as int) == spec_ycocg_forward(
            r as int,
            g as int,
            b as int,
        ),
        -318 <= out.0 <= 573,
        -255 <= out.1 <= 255,
        -382 <= out.2 <= 382,
{
    let co: i16 = r as i16 - b as i16;
    let t: i16 = b as i16 + div2_trunc(co);
    let cg: i16 = g as i16 - t;
    let y: i16 = t + div2_trunc(cg);
    (y, co, cg)
}

/// Inverse YCoCg for one pixel of *encoder-produced* values (see the
/// ranges in `ycocg_forward_pixel`). The decoder proper keeps its
/// i32-widened, clamping version because it must also accept hostile
/// values; this function documents and verifies the lossless case.
pub fn ycocg_inverse_pixel(y: i16, co: i16, cg: i16) -> (out: (i16, i16, i16))
    requires
        -1000 <= y <= 1000,
        -1000 <= co <= 1000,
        -1000 <= cg <= 1000,
    ensures
        (out.0 as int, out.1 as int, out.2 as int) == spec_ycocg_inverse(
            y as int,
            co as int,
            cg as int,
        ),
{
    let t: i16 = y - div2_trunc(cg);
    let g: i16 = cg + t;
    let b: i16 = t - div2_trunc(co);
    let r: i16 = co + b;
    (r, g, b)
}

// ---------------------------------------------------------------------
// Tile geometry (deepmap2.md "Tiling")
//
// Tiles are full-width horizontal strips whose raw byte size must fit the
// per-compression budget (1,044,480 or 2,097,152 bytes), except that a
// single row is always allowed even when one row alone exceeds the
// budget.
// ---------------------------------------------------------------------

/// Rows per tile for a raw-byte budget. Verified: the result is a valid
/// strip height (1..=height), and it respects the budget unless a single
/// row already exceeds it.
pub fn tile_rows_for_budget(budget: usize, row_bytes: usize, height: u32) -> (h: u32)
    requires
        budget >= 1,
        row_bytes >= 1,
        height >= 1,
        budget <= u32::MAX as usize,
    ensures
        1 <= h <= height,
        (h as int) * (row_bytes as int) <= budget as int || h == 1,
{
    let max_rows: usize = budget / row_bytes;
    proof {
        // budget == row_bytes * q + (budget % row_bytes) with the
        // remainder in [0, row_bytes), hence q * row_bytes <= budget.
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(
            budget as int,
            row_bytes as int,
        );
        vstd::arithmetic::div_mod::lemma_mod_bound(budget as int, row_bytes as int);
        vstd::arithmetic::div_mod::lemma_div_nonincreasing(budget as int, row_bytes as int);
        assert(max_rows as int == budget as int / row_bytes as int);
        assert(budget as int == row_bytes as int * (budget as int / row_bytes as int)
            + budget as int % row_bytes as int);
        assert(budget as int % row_bytes as int >= 0);
        assert((max_rows as int) * (row_bytes as int) <= budget as int);
        assert(max_rows <= budget);
    }
    if max_rows == 0 {
        1
    } else if (height as usize) <= max_rows {
        proof {
            vstd::arithmetic::mul::lemma_mul_inequality(
                height as int,
                max_rows as int,
                row_bytes as int,
            );
        }
        height
    } else {
        proof {
            assert(max_rows <= u32::MAX as usize);
        }
        max_rows as u32
    }
}

} // verus!
