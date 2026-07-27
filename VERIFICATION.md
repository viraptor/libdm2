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

`src/verified.rs` is production code (the encode/decode paths call it)
and carries Verus specs, checked by `./verify.sh`. Current status:
**20 verified, 0 errors** with Verus `0.2026.07.25.d64f7c4` (the commit
pinned as `VERUS_GIT_REV` in `verify.sh`):

| Item | Property | Proof style |
|---|---|---|
| `zigzag_encode`/`zigzag_decode` | mutually inverse on all 2^16 values (bijection) | 16-bit `bit_vector` |
| `adjust_residual`/`unadjust_residual` | mutually inverse for every `res > i16::MIN`; unadjust total | linear arithmetic; single-definition style (`#[verifier::allow_in_spec]` + `returns`), so the exec functions are their own spec |
| `lemma_residual_pipeline_roundtrip` | the full type-2 residual pipeline (adjust → zigzag → unzigzag → unadjust) is the identity | composition |
| `wrap_add_i16` | two's-complement wrap-around addition, per vstd's trusted `i16_specs::wrapping_add` | vstd spec |
| `unpredict_none`/`unpredict_left`/`unpredict_up` | elementwise functional postconditions for the three encoder-emitted prediction modes | loop invariants |

Every proved property also has an executable counterpart in
`tests/verified_props.rs` (exhaustive where the domain allows — all 65536
zigzag values, the full 2^24 RGB cube for colorspace), so the guarantees
are enforced by plain `cargo test` on machines without Verus, and the
verified implementations are pinned against the historical formulas they
replaced.

The same test file adds hostile-input validation for the tiers that are
not yet proved: LZVN decode fuzzing (arbitrary bytes, truncations,
bit-flips, undersized outputs), container-level corruption sweeps, and an
`dm2_encode_bound` property check. Writing it immediately found two
tier-1 violations — debug-build overflow panics in the multi-channel
type-2 decoder on corrupted streams (i16 prediction sums and inverse-YCoCg
intermediates), both fixed on this branch using the verified
`wrap_add_i16` and i32 widening respectively.

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

1. **LZVN decoder, tier 1**: prove the decode loop panic-free and in-bounds
   for arbitrary input (loop invariants over `sp`/`dp`, termination by
   `src.len() - sp`). Rewrites `copy_literal`/`copy_match` with verified
   contracts.
2. **`UpLeft`/`Mean` reconstruction**: extend the verified row functions to
   the remaining two modes (i32 intermediates are in the supported subset)
   and converge `predict::unpredict_row` onto them once Verus's `&mut`
   slice support allows writing through the caller's buffer.
3. **Colorspace, tier 2**: the truncating-division YCoCg variant, either
   via `bit_vector` on a division-free reformulation or exhaustive-by-proof
   over u8 triples. (Currently: exhaustively tested, not proved.)
4. **Predict→unpredict round-trip, tier 2**: verify the encoder-side
   residual computation as the inverse of the verified reconstruction.
5. **`Header::read`/`write`, tiers 1+3**: inverse pair + panic freedom on
   hostile headers.
6. **LZVN encoder→decoder round-trip, tier 4**: the long game; would make
   the entire sub-4096-byte tile path (the pure-Rust one) end-to-end
   verified.
