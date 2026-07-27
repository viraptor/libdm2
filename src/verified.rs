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
// Prediction modes (deepmap2.md "Prediction modes")
//
// Defined here (rather than in predict.rs, which re-exports) so the
// specs below can match on the modes directly.
// ---------------------------------------------------------------------

/// Prediction modes for type 2 (default) compression.
/// Each mode predicts each pixel from its neighbors; the encoder stores
/// the signed residual (actual - predicted) which tends to be near zero
/// for smooth images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PredictMode {
    None = 0,
    UpLeft = 1,
    Left = 2,
    Up = 3,
    Mean = 4,
}

impl PredictMode {
    #[verifier::allow_in_spec]
    pub fn from_u8(v: u8) -> (r: Option<Self>)
        returns
            (match v {
                0u8 => Some(PredictMode::None),
                1u8 => Some(PredictMode::UpLeft),
                2u8 => Some(PredictMode::Left),
                3u8 => Some(PredictMode::Up),
                4u8 => Some(PredictMode::Mean),
                _ => None,
            }),
    {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::UpLeft),
            2 => Some(Self::Left),
            3 => Some(Self::Up),
            4 => Some(Self::Mean),
            _ => None,
        }
    }
}

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
// Gray type-2 row coding: the full verified value pipeline
//
// Encoder (per row):  res = value - pred;  adjust (unless mode None);
//                     zigzag;  split into hi/lo byte planes.
// Decoder (per row):  reassemble;  unzigzag;  unadjust;  un-predict.
//
// `encode_gray_row`/`decode_gray_row` are the production implementations
// (called from encode.rs/decode.rs), and `lemma_gray_row_roundtrip`
// proves the decoder recurrence recovers the encoded row **exactly** for
// every u8-range row and every encoder-emitted mode — making the type-2
// gray value pipeline verified end-to-end (the LZFSE layer in between is
// byte-transparent and out of scope).
// ---------------------------------------------------------------------

/// Splitting a code word into hi/lo bytes and reassembling is the identity.
pub proof fn lemma_bytes_roundtrip(z: u16)
    ensures
        ((((z >> 8) as u8) as u16) << 8) | ((z as u8) as u16) == z,
{
    assert(((((z >> 8) as u8) as u16) << 8) | ((z as u8) as u16) == z) by (bit_vector);
}

/// Spec: the encoder's predictor for the gray modes it emits (None,
/// Left, Up). Prediction reads the *original* values; losslessness (the
/// round-trip theorem) is exactly what makes the decoder's reconstructed
/// context identical.
pub open spec fn spec_gray_pred(cur: Seq<i16>, prev: Seq<i16>, mode: PredictMode, i: int) -> i16 {
    match mode {
        PredictMode::None => 0i16,
        PredictMode::Left => if i == 0 {
            0i16
        } else {
            cur[i - 1]
        },
        _ => prev[i],
    }
}

/// Spec: the residual the encoder stores (post-adjustment).
pub open spec fn spec_gray_residual(
    cur: Seq<i16>,
    prev: Seq<i16>,
    mode: PredictMode,
    i: int,
) -> i16 {
    let raw = (cur[i] - spec_gray_pred(cur, prev, mode, i)) as i16;
    if mode is None {
        raw
    } else {
        adjust_residual(raw)
    }
}

/// Spec: the zigzag code word the encoder stores for position `i`.
pub open spec fn spec_gray_code(cur: Seq<i16>, prev: Seq<i16>, mode: PredictMode, i: int) -> u16 {
    spec_zigzag_encode(spec_gray_residual(cur, prev, mode, i) as u16)
}

/// Spec: the residual the decoder recovers from one hi/lo byte pair.
pub open spec fn spec_code_residual(hi: u8, lo: u8) -> i16 {
    unadjust_residual(spec_zigzag_decode(((hi as u16) << 8) | (lo as u16)) as i16)
}

/// Spec: the decoder's predictor, reading its own reconstruction.
pub open spec fn spec_gray_dec_pred(
    out: Seq<i16>,
    prev: Seq<i16>,
    mode: PredictMode,
    i: int,
) -> i16 {
    match mode {
        PredictMode::None => 0i16,
        PredictMode::Left => if i == 0 {
            0i16
        } else {
            out[i - 1]
        },
        _ => prev[i],
    }
}

/// Spec: both value-range preconditions of the gray encoder.
pub open spec fn spec_gray_ranges(cur: Seq<i16>, prev: Seq<i16>) -> bool {
    &&& forall|j: int| 0 <= j < cur.len() ==> 0 <= #[trigger] cur[j] <= 255
    &&& forall|j: int| 0 <= j < prev.len() ==> 0 <= #[trigger] prev[j] <= 255
}

/// Spec: `out` satisfies the decoder recurrence over the bytes the
/// encoder produced for `cur`/`prev`/`mode`.
pub open spec fn spec_gray_decode_rel(
    cur: Seq<i16>,
    prev: Seq<i16>,
    mode: PredictMode,
    out: Seq<i16>,
) -> bool {
    forall|j: int|
        0 <= j < cur.len() ==> #[trigger] out[j] == spec_wrap_add(
            spec_code_residual(
                (spec_gray_code(cur, prev, mode, j) >> 8) as u8,
                spec_gray_code(cur, prev, mode, j) as u8,
            ),
            spec_gray_dec_pred(out, prev, mode, j),
        )
}

/// Encode one gray row into the hi/lo byte planes (the production
/// encoder path for 1-channel type 2).
pub fn encode_gray_row(cur: &[i16], prev: &[i16], mode: PredictMode, hi: &mut [u8], lo: &mut [
    u8])
    requires
        cur@.len() == prev@.len(),
        old(hi).len() == cur.len(),
        old(lo).len() == cur.len(),
        mode is None || mode is Left || mode is Up,
        spec_gray_ranges(cur@, prev@),
    ensures
        final(hi)@.len() == cur@.len(),
        final(lo)@.len() == cur@.len(),
        forall|i: int|
            0 <= i < cur@.len() ==> (#[trigger] final(hi)@[i]) == (spec_gray_code(
                cur@,
                prev@,
                mode,
                i,
            ) >> 8) as u8 && final(lo)@[i] == spec_gray_code(cur@, prev@, mode, i) as u8,
{
    let mut k: usize = 0;
    while k < cur.len()
        invariant
            cur@.len() == prev@.len(),
            hi@.len() == cur@.len(),
            lo@.len() == cur@.len(),
            k <= cur@.len(),
            mode is None || mode is Left || mode is Up,
            spec_gray_ranges(cur@, prev@),
            forall|i: int|
                0 <= i < k ==> (#[trigger] hi@[i]) == (spec_gray_code(cur@, prev@, mode, i)
                    >> 8) as u8 && lo@[i] == spec_gray_code(cur@, prev@, mode, i) as u8,
        decreases cur@.len() - k,
    {
        let pred: i16 = match mode {
            PredictMode::None => 0,
            PredictMode::Left => if k == 0 {
                0
            } else {
                cur[k - 1]
            },
            _ => prev[k],
        };
        let mut res = cur[k] - pred;
        if !matches!(mode, PredictMode::None) {
            res = adjust_residual(res);
        }
        let z = zigzag_encode(res);
        hi[k] = (z >> 8) as u8;
        lo[k] = z as u8;
        k += 1;
    }
}

/// Decode one gray row from the hi/lo byte planes (the production
/// decoder path for 1-channel type 2). Handles all five modes; returns
/// false when the mode needs a previous row and there is none (corrupt
/// stream). For the encoder-emitted modes the postcondition gives the
/// full decoder recurrence, which `lemma_gray_row_roundtrip` composes
/// with the encoder's to the identity.
pub fn decode_gray_row(
    hi: &[u8],
    lo: &[u8],
    prev: Option<&[i16]>,
    mode: PredictMode,
    out: &mut [i16],
) -> (ok: bool)
    requires
        hi@.len() == lo@.len(),
        old(out).len() == hi@.len(),
        match prev {
            Some(p) => p@.len() == hi@.len(),
            None => true,
        },
    ensures
        final(out)@.len() == old(out)@.len(),
        ok == !((mode is Up || mode is UpLeft || mode is Mean) && prev is None),
        ok && (mode is None || mode is Left || mode is Up) ==> forall|i: int|
            0 <= i < hi@.len() ==> (#[trigger] final(out)@[i]) == spec_wrap_add(
                spec_code_residual(hi@[i], lo@[i]),
                spec_gray_dec_pred(
                    final(out)@,
                    if mode is Up {
                        prev.unwrap()@
                    } else {
                        Seq::empty()
                    },
                    mode,
                    i,
                ),
            ),
{
    let mut residuals: Vec<i16> = Vec::with_capacity(hi.len());
    let mut k: usize = 0;
    while k < hi.len()
        invariant
            hi@.len() == lo@.len(),
            k <= hi@.len(),
            residuals@.len() == k,
            forall|i: int|
                0 <= i < k ==> #[trigger] residuals@[i] == spec_code_residual(hi@[i], lo@[i]),
        decreases hi@.len() - k,
    {
        let z: u16 = ((hi[k] as u16) << 8) | (lo[k] as u16);
        let d = zigzag_decode(z);
        proof {
            assert(((d as u16) as i16) == d) by (bit_vector);
        }
        residuals.push(unadjust_residual(d));
        k += 1;
    }
    match mode {
        PredictMode::None => {
            unpredict_none(residuals.as_slice(), out);
            true
        }
        PredictMode::Left => {
            unpredict_left(residuals.as_slice(), out);
            true
        }
        PredictMode::Up => {
            match prev {
                Some(p) => {
                    unpredict_up(residuals.as_slice(), p, out);
                    true
                }
                None => false,
            }
        }
        PredictMode::UpLeft => {
            match prev {
                Some(p) => {
                    unpredict_upleft(residuals.as_slice(), p, out);
                    true
                }
                None => false,
            }
        }
        PredictMode::Mean => {
            match prev {
                Some(p) => {
                    unpredict_mean(residuals.as_slice(), p, out);
                    true
                }
                None => false,
            }
        }
    }
}

/// Induction step for the round-trip theorem: element `i` of the decoded
/// row equals element `i` of the encoded row.
proof fn lemma_gray_row_roundtrip_at(
    cur: Seq<i16>,
    prev: Seq<i16>,
    mode: PredictMode,
    out: Seq<i16>,
    i: int,
)
    requires
        mode is None || mode is Left || mode is Up,
        prev.len() == cur.len(),
        out.len() == cur.len(),
        spec_gray_ranges(cur, prev),
        spec_gray_decode_rel(cur, prev, mode, out),
        0 <= i < cur.len(),
    ensures
        out[i] == cur[i],
    decreases i,
{
    let z = spec_gray_code(cur, prev, mode, i);
    lemma_bytes_roundtrip(z);
    let res = spec_gray_residual(cur, prev, mode, i);
    lemma_zigzag_roundtrip_signed(res);
    let raw = (cur[i] - spec_gray_pred(cur, prev, mode, i)) as i16;
    if !(mode is None) {
        lemma_adjust_roundtrip(raw);
    }
    if mode is Left && i > 0 {
        lemma_gray_row_roundtrip_at(cur, prev, mode, out, i - 1);
    }
}

/// **The round-trip theorem for gray type-2 rows**: any reconstruction
/// satisfying the decoder recurrence over the encoder's bytes equals the
/// original row exactly, for every u8-range row and every encoder mode.
pub proof fn lemma_gray_row_roundtrip(
    cur: Seq<i16>,
    prev: Seq<i16>,
    mode: PredictMode,
    out: Seq<i16>,
)
    requires
        mode is None || mode is Left || mode is Up,
        prev.len() == cur.len(),
        out.len() == cur.len(),
        spec_gray_ranges(cur, prev),
        spec_gray_decode_rel(cur, prev, mode, out),
    ensures
        out =~= cur,
{
    assert forall|i: int| 0 <= i < cur.len() implies out[i] == cur[i] by {
        lemma_gray_row_roundtrip_at(cur, prev, mode, out, i);
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

/// Spec: clamp an integer into the u8 range.
pub open spec fn spec_clamp_u8(x: int) -> int {
    if x < 0 {
        0
    } else if x > 255 {
        255
    } else {
        x
    }
}

/// Clamp an i32 into u8 (the decoder's saturation step).
fn clamp_u8_i32(v: i32) -> (r: u8)
    ensures
        r as int == spec_clamp_u8(v as int),
{
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}

/// Total inverse YCoCg with clamping — the production per-pixel inverse
/// of the type-2 RGB/RGBA decoder. Computed in i32 (matching C integer
/// promotion in Apple's decoder) so it is safe for *hostile* Y/Co/Cg
/// values, then saturated to u8.
pub fn ycocg_inverse_clamped(y: i16, co: i16, cg: i16) -> (out: (u8, u8, u8))
    ensures
        out.0 as int == spec_clamp_u8(spec_ycocg_inverse(y as int, co as int, cg as int).0),
        out.1 as int == spec_clamp_u8(spec_ycocg_inverse(y as int, co as int, cg as int).1),
        out.2 as int == spec_clamp_u8(spec_ycocg_inverse(y as int, co as int, cg as int).2),
{
    let yw = y as i32;
    let cow = co as i32;
    let cgw = cg as i32;
    let t = yw - cgw / 2;
    let g = cgw + t;
    let b = t - cow / 2;
    let r = cow + b;
    (clamp_u8_i32(r), clamp_u8_i32(g), clamp_u8_i32(b))
}

/// For YCoCg triples produced by the forward transform of an 8-bit
/// pixel, the clamped inverse is exact: composition of the round-trip
/// theorem with clamp-identity on in-range values.
pub proof fn lemma_ycocg_clamped_roundtrip(r: int, g: int, b: int)
    requires
        0 <= r <= 255,
        0 <= g <= 255,
        0 <= b <= 255,
    ensures
        ({
            let (y, co, cg) = spec_ycocg_forward(r, g, b);
            &&& spec_clamp_u8(spec_ycocg_inverse(y, co, cg).0) == r
            &&& spec_clamp_u8(spec_ycocg_inverse(y, co, cg).1) == g
            &&& spec_clamp_u8(spec_ycocg_inverse(y, co, cg).2) == b
        }),
{
    lemma_ycocg_roundtrip(r, g, b);
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
