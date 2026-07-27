# Formal verification of libdm2 with Verus

This document lays out what formal verification can and cannot buy for a
codec like this one, what is implemented today, and the roadmap. The
verified code lives in [`src/verified.rs`](src/verified.rs); run
[`./verify.sh`](verify.sh) to check it with
[Verus](https://github.com/verus-lang/verus).

## What can you verify in an encoder/decoder beyond "won't crash"?

Quite a lot, and the properties form a natural ladder. For a codec the
interesting statements are, in increasing order of strength:

**Tier 0 — memory safety.** Free from safe Rust, except at the two
`unsafe` boundaries: the LZFSE C FFI (`src/lzfse.rs`) and the exported C
ABI (`src/ffi.rs`). Verus does not help across FFI; those stay trusted.

**Tier 1 — panic freedom / total decoding.** For every input byte string
(including hostile ones), the decoder terminates and returns `Ok`/`Err` —
never panics, never overflows in debug builds, never allocates an
attacker-controlled amount unchecked. This sounds like "won't crash" but
is *stronger* than what testing gives: it's a proof over all 2^8n inputs,
not the ones a fuzzer happened to visit. This tier is where a verifier
earns its keep on parser-shaped code (`Header::read`, tile walking, the
LZVN opcode loop).

**Tier 2 — functional correctness of the pure primitives.** The codec's
arithmetic core is made of small bijections and their inverses:

- zigzag coding: `decode(encode(x)) == x` and `encode(decode(z)) == z`
- the deepmap2 negative-residual adjustment: `unadjust(adjust(r)) == r`
- prediction: `unpredict_mode(predict_mode(row)) == row` for each mode
- colorspace: `ycocg_to_rgb(rgb_to_ycocg(p)) == p` for all p in the 8-bit cube

These are exactly the properties whose violation silently corrupts pixels
rather than crashing, so tests catch them late or never. They are also
the easiest proofs: scalar bit-vector and linear-arithmetic goals that
Z3 discharges automatically.

**Tier 3 — structural invariants of the container.** `Header::write` and
`Header::read` are mutually inverse; tile geometry covers every row
exactly once; `dm2_encode_bound` really bounds every encoder output
(today it's a heuristic with a test, not a theorem).

**Tier 4 — end-to-end round-trip.** For every valid image,
`decode(encode(img)) == img`. Compositional consequence of tiers 2–3
*plus* a round-trip theorem for the entropy-coding layer. For the
pure-Rust LZVN path this is a real (ambitious) target; for the LZFSE
`bvx2` path it is out of reach — that code is C behind FFI, so the
compression layer stays a trusted, byte-transparent component and the
end-to-end statement becomes conditional on it.

**Not provable at all: Apple compatibility.** "Bug-for-bug compatible
with Apple's implementation" is a relation to an external black box.
No spec, no proof — only differential testing against Apple's output
(`tests/cross_validate.rs`) can speak to it. Formal verification here
proves *internal* coherence (our decoder inverts our encoder; our decoder
is total on hostile input), which is complementary.

## How far can we go, given the current implementation?

Constraints discovered while doing this:

- **`src/lzfse.rs`** — C FFI, unverifiable, stays trusted. Its callers can
  at most be verified to pass consistent buffer sizes.
- **`predict_row`'s mode heuristic uses `f32`** — Verus has no float
  support. Fortunately the heuristic is *correctness-irrelevant by
  design*: whatever mode it picks, the decoder must invert it. Verifying
  the inversions (and leaving the cost function unverified) captures all
  the correctness content.
- **`encode_palette` uses `HashMap`** — outside the comfortable vstd
  subset; would need restructuring to verify.
- **`&mut [T]` support in Verus is still limited**, which shapes the
  row-reconstruction API (the verified functions return `Vec` rather than
  writing through a caller slice).
- **The LZVN decoder (`src/lzvn.rs`) is verifiable** — pure slices,
  bounded loops, no floats, no FFI. It is the single highest-value future
  target because it parses untrusted bytes with nontrivial index math.

## What is verified today

`src/verified.rs` and `src/verified_lzvn.rs` are production code (the
encode/decode paths call them) and carry Verus specs, checked by
`./verify.sh`. Current status: **52 verified, 0 errors** with Verus
`0.2026.07.25.d64f7c4` (the commit pinned as `VERUS_GIT_REV` in
`verify.sh`):

| Item | Property | Proof style |
|---|---|---|
| `zigzag_encode`/`zigzag_decode` | mutually inverse on all 2^16 values (bijection); implementation equals the arithmetic formula documented in deepmap2.md (`x >= 0 ? 2x : -2x-1`) | 16-bit `bit_vector` |
| `adjust_residual`/`unadjust_residual` | mutually inverse for every `res > i16::MIN`; unadjust total | linear arithmetic; single-definition style (`#[verifier::allow_in_spec]` + `returns`), so the exec functions are their own spec |
| `lemma_residual_pipeline_roundtrip` | the full type-2 residual pipeline (adjust → zigzag → unzigzag → unadjust) is the identity | composition |
| `wrap_add_i16` | two's-complement wrap-around addition, per vstd's trusted `i16_specs::wrapping_add` | vstd spec |
| `unpredict_{none,left,up,upleft,mean}` | elementwise functional postconditions for **all five** documented prediction-mode inversions (2-way Paeth and Mean included), writing through the caller's `&mut [i16]`; `predict::unpredict_row` is now only mode dispatch | loop invariants over mutable slices |
| `ycocg_forward_pixel`/`ycocg_inverse_pixel` | exact round-trip of the truncating-division YCoCg transform — proved for *all* integer inputs, not just the 8-bit cube — plus output range bounds | linear (Co/Cg pass through; the halved terms cancel) |
| `tile_rows_for_budget` | tile strip height is in `1..=height` and respects the raw-byte budget unless a single row alone exceeds it (deepmap2.md "Tiling") | vstd div/mod lemmas |
| `verified_lzvn::decode` | **the production LZVN decoder**: panic-free (all indexing in bounds, no over/underflow), terminating, output length ≤ buffer, buffer length preserved — for arbitrary hostile input | loop invariants + decreases |
| `encode_gray_row`/`decode_gray_row` | the production gray type-2 row coders: full functional postconditions (predict → adjust → zigzag → hi/lo byte split, and its inverse); the decoder is total on hostile bytes | loop invariants |
| **`lemma_gray_row_roundtrip`** | **the row round-trip theorem**: for every u8-range row and every encoder-emitted mode, the decoder recurrence over the encoder's bytes reconstructs the row *exactly* — the gray type-2 value pipeline is verified end-to-end (LZFSE in between is byte-transparent) | induction composing the byte-split, zigzag, adjustment, and wrap-add lemmas |
| `ycocg_inverse_clamped` | the production per-pixel inverse for RGB/RGBA type-2 decode: total on hostile values (i32-widened, saturating), and exact on encoder-produced values (`lemma_ycocg_clamped_roundtrip`) | linear + clamp identity |
| `PredictMode::from_u8` | mode-byte parsing, single-definition style | `allow_in_spec` |

Every proved property also has an executable counterpart in
`tests/verified_props.rs` (exhaustive where the domain allows — all 65536
zigzag values, the full 2^24 RGB cube for colorspace), so the guarantees
are enforced by plain `cargo test` on machines without Verus. Where a
verified implementation replaced existing code (all five unpredict modes,
the LZVN decoder, tile height), the pre-verification implementation is
kept verbatim in the test file as a reference oracle and
differential-tested — valid, truncated, bit-flipped, and random streams
must produce byte-identical results.

The hostile-input fuzz suite in the same file found two tier-1 violations
on day one — debug-build overflow panics in the multi-channel type-2
decoder on corrupted streams (i16 prediction sums and inverse-YCoCg
intermediates), both fixed on this branch using the verified
`wrap_add_i16` and i32 widening respectively.

Notes for future proof work in this codebase, learned the hard way:
truncating `as`-casts and bit-level facts are only exported to the
`bit_vector`/`compute` solvers (state them in a `by (bit_vector)` assert,
then let the default solver chain them); a slice view's `len()` is an
unbounded `nat` inside loop invariants, so carry `dst@.len() <= usize::MAX`
(or bind an exec `len()`) when index arithmetic must not overflow; and
keep bit-ops in `u8` and add constants only after casting to `usize`, so
type bounds make overflow obligations trivial.

## Build/verify mechanics

- `cargo build`/`cargo test` need no Verus: `vstd` is a normal crates.io
  dependency and all ghost code erases at compile time.
- `./verify.sh` runs the Verus binary (downloading a release if needed)
  against `verify/verified_shim.rs`, which mounts only `src/verified.rs`.
  The rest of the crate (FFI, floats, HashMap) is invisible to the
  verifier — deliberately.
- `./verify.sh --from-source` clones and builds the verifier instead
  (rustup toolchain pinned by Verus's `rust-toolchain.toml`, plus a Z3
  4.12.5 binary from PATH, `get-z3.sh`, or the PyPI `z3-solver` wheel).
  Use it where GitHub *release* downloads are unavailable but git clones
  work, or to pin an exact verifier commit via `VERUS_GIT_REV`.
- The verifier **cannot** be a Cargo dependency: the `verus` and
  `cargo-verus` names on crates.io are empty placeholders (their stub
  binary exits 0 on any invocation — `verify.sh` detects and rejects it).
  Only the libraries (`vstd`, `verus_builtin*`) are published for real.
- The `vstd` pin in `Cargo.toml` should match the Verus release date used
  by `verify.sh` (both currently 2026-07-12-ish); see comments in both.

## Roadmap (in rough order of value per effort)

1. ~~**LZVN decoder, tier 1**~~ — **done**: `verified_lzvn::decode` is the
   production decoder, proved panic-free and terminating on arbitrary
   input.
2. ~~**`UpLeft`/`Mean` reconstruction**~~ — **done**: all five modes are
   verified and `predict::unpredict_row` is pure dispatch (Verus's
   first-class `&mut` slice support made the production signature
   directly verifiable).
3. ~~**Colorspace, tier 2**~~ — **done** for the scalar transform used by
   the encoder (`ycocg_forward_pixel`/`ycocg_inverse_pixel`, round-trip
   proved for all integers). The decoder's i32-widened clamping loop is
   intentionally separate (it must accept hostile values) and remains
   test-covered only.
4. ~~**Predict→unpredict round-trip, tier 2**~~ — **done** for the gray
   pipeline: `lemma_gray_row_roundtrip` proves decode ∘ encode is the
   identity at the row level, over the actual hi/lo byte planes, for all
   encoder-emitted modes, and the production row coders carry the specs
   it composes. Multi-channel rows currently only use mode 0 (None) plus
   the proved YCoCg/residual scalar round-trips; extending the row
   theorem to the interleaved multi-channel layout is the natural next
   increment.
5. **`Header::read`/`write`, tiers 1+3**: inverse pair + panic freedom on
   hostile headers (needs byte-serialization specs for the u16 LE fields
   and the palette block).
6. **Decode tile walk, tier 1**: `decode_tiled`'s offset arithmetic over
   the `[u32 size][data]` framing, closing the last unverified index math
   between the container and the verified layers.
7. **LZVN decoder functional spec, tier 2**: strengthen from "safe" to
   "decodes correctly", giving meaning to the opcode table itself.
8. **LZVN encoder→decoder round-trip, tier 4**: the long game; would make
   the entire sub-4096-byte tile path (the pure-Rust one) end-to-end
   verified.
