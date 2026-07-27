//! Crate root used by the Verus verifier (see ../verify.sh).
//!
//! Verus cannot check the whole libdm2 crate — it contains C FFI (LZFSE),
//! floating-point heuristics, and std collections outside the supported
//! subset. This shim mounts only the verified module, so
//! `verus --crate-type=lib verify/verified_shim.rs` checks exactly the
//! code that carries specifications, using the vstd bundled with the
//! Verus release.
//!
//! The same source file compiles as `libdm2::verified` in the normal
//! cargo build, so what is verified is what ships.

#[path = "../src/verified.rs"]
pub mod verified;

#[path = "../src/verified_lzvn.rs"]
pub mod verified_lzvn;
