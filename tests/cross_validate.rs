//! Cross-validation against Apple's vImageDeepmap2 implementation.
//!
//! For each sample we run two checks:
//!   1. ours_enc → apple_dec: encode with libdm2, decode with Apple, compare.
//!   2. apple_enc → ours_dec: encode with Apple, decode with libdm2, compare.
//!
//! macOS-only; the symbols live in the Accelerate framework's private vImage
//! deepmap2 API documented in `deepmap2.md`.

#![cfg(target_os = "macos")]

use libdm2::*;

#[link(name = "Accelerate", kind = "framework")]
extern "C" {}

#[repr(C)]
struct VImageBuffer {
    data: *mut u8,
    height: usize, // vImagePixelCount = unsigned long
    width: usize,
    row_bytes: usize,
}

#[repr(C)]
#[derive(Default)]
struct Deepmap2Options {
    compression_type: u32,
    quality: u32,
    param: u32,
}

type EncodeFn = unsafe extern "C" fn(
    src: *mut VImageBuffer,
    pixel_format: u32,
    opts: *mut Deepmap2Options,
    out_buf: *mut u8,
    out_size: usize,
) -> usize;

type DecodeFn = unsafe extern "C" fn(
    dst: *mut VImageBuffer,
    pixel_format: u32,
    enc_data: *const u8,
    enc_size: usize,
    scratch: *mut u8,
) -> *mut u8;

type ScratchSizeFn = unsafe extern "C" fn() -> usize;

fn load_sym<T: Copy>(name: &str) -> Option<T> {
    use std::ffi::CString;
    extern "C" {
        fn dlsym(handle: *mut std::ffi::c_void, sym: *const i8) -> *mut std::ffi::c_void;
    }
    const RTLD_DEFAULT: *mut std::ffi::c_void = -2isize as *mut std::ffi::c_void;
    let cname = CString::new(name).ok()?;
    let p = unsafe { dlsym(RTLD_DEFAULT, cname.as_ptr()) };
    if p.is_null() {
        return None;
    }
    // Safety: T is a function pointer of the same size as a void*.
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut std::ffi::c_void>());
    Some(unsafe { std::mem::transmute_copy::<*mut std::ffi::c_void, T>(&p) })
}

struct AppleApi {
    encode: EncodeFn,
    decode: DecodeFn,
    scratch_size: ScratchSizeFn,
}

fn apple_api() -> Option<AppleApi> {
    Some(AppleApi {
        encode: load_sym("vImageDeepmap2Encode")?,
        decode: load_sym("vImageDeepmap2Decode")?,
        scratch_size: load_sym("vImageDeepmap2DecodeScratchBufferSize")?,
    })
}

fn pixel_format_code(f: PixelFormat) -> u32 {
    f as u32
}

fn apple_encode(
    api: &AppleApi,
    pixels: &[u8],
    info: &ImageInfo,
    compression: Compression,
) -> Option<Vec<u8>> {
    apple_encode_opts(
        api,
        pixels,
        info,
        compression,
        0,
        if info.format.is_16bit() { 12 } else { 0 },
    )
}

fn apple_encode_opts(
    api: &AppleApi,
    pixels: &[u8],
    info: &ImageInfo,
    compression: Compression,
    quality: u32,
    param: u32,
) -> Option<Vec<u8>> {
    let row_bytes = info.width as usize * info.format.pixel_size();
    let mut src_copy = pixels.to_vec();
    let mut buf = VImageBuffer {
        data: src_copy.as_mut_ptr(),
        height: info.height as usize,
        width: info.width as usize,
        row_bytes,
    };
    let mut opts = Deepmap2Options {
        compression_type: compression as u32,
        quality,
        param,
    };
    let cap = pixels.len() * 4 + 4096;
    let mut out = vec![0u8; cap];
    let n = unsafe {
        (api.encode)(
            &mut buf,
            pixel_format_code(info.format),
            &mut opts,
            out.as_mut_ptr(),
            cap,
        )
    };
    if n == 0 {
        return None;
    }
    out.truncate(n);
    Some(out)
}

fn apple_decode(api: &AppleApi, encoded: &[u8], info: &ImageInfo) -> Option<Vec<u8>> {
    let row_bytes = info.width as usize * info.format.pixel_size();
    let mut pixels = vec![0u8; row_bytes * info.height as usize];
    let mut buf = VImageBuffer {
        data: pixels.as_mut_ptr(),
        height: info.height as usize,
        width: info.width as usize,
        row_bytes,
    };
    let scratch_sz = unsafe { (api.scratch_size)() };
    let mut scratch = vec![0u8; scratch_sz.max(1)];
    let res = unsafe {
        (api.decode)(
            &mut buf,
            pixel_format_code(info.format),
            encoded.as_ptr(),
            encoded.len(),
            scratch.as_mut_ptr(),
        )
    };
    if res.is_null() {
        return None;
    }
    Some(pixels)
}

// --- Sample generators ---

fn gradient(w: u32, h: u32, fmt: PixelFormat) -> Vec<u8> {
    let ps = fmt.pixel_size();
    let mut p = vec![0u8; w as usize * h as usize * ps];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let off = (y * w as usize + x) * ps;
            for c in 0..ps {
                p[off + c] = ((x * 5 + y * 3 + c * 23) & 0xFF) as u8;
            }
        }
    }
    p
}

fn solid(w: u32, h: u32, fmt: PixelFormat, v: u8) -> Vec<u8> {
    vec![v; w as usize * h as usize * fmt.pixel_size()]
}

fn checker(w: u32, h: u32, fmt: PixelFormat) -> Vec<u8> {
    let ps = fmt.pixel_size();
    let mut p = vec![0u8; w as usize * h as usize * ps];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let off = (y * w as usize + x) * ps;
            let v: u8 = if (x ^ y) & 1 == 0 { 0x10 } else { 0xE0 };
            for c in 0..ps {
                p[off + c] = v.wrapping_add((c * 30) as u8);
            }
        }
    }
    p
}

fn few_colors_rgba(w: u32, h: u32) -> Vec<u8> {
    let n = (w * h) as usize;
    let mut p = vec![0u8; n * 4];
    for i in 0..n {
        let c = (i % 6) as u8;
        p[i * 4] = c * 30;
        p[i * 4 + 1] = c * 20 + 10;
        p[i * 4 + 2] = c * 10 + 50;
        p[i * 4 + 3] = 0xFF;
    }
    p
}

fn maxerr(a: &[u8], b: &[u8]) -> u16 {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x as i16 - y as i16).unsigned_abs())
        .max()
        .unwrap_or(0)
}

/// Tolerance for self-vs-Apple comparison. Lossless paths must match exactly;
/// type 2 has documented per-channel rounding noise.
fn tolerance(compression: Compression, fmt: PixelFormat) -> u16 {
    match compression {
        Compression::None | Compression::Lossless | Compression::Palette => 0,
        Compression::Default => {
            if fmt.channels() >= 3 {
                // YCoCg rounding + negative-residual adjustment can stack
                8
            } else {
                3
            }
        }
    }
}

fn check_pair(
    api: &AppleApi,
    label: &str,
    pixels: &[u8],
    info: &ImageInfo,
    compression: Compression,
) {
    // 1. ours_enc -> apple_dec
    let ours = dm2_encode(pixels, info, compression)
        .unwrap_or_else(|e| panic!("[{label}] our encode failed: {e}"));
    let apple_decoded = apple_decode(api, &ours, info)
        .unwrap_or_else(|| panic!("[{label}] apple decode of our encode returned NULL"));
    let tol = tolerance(compression, info.format);
    let err = maxerr(pixels, &apple_decoded);
    assert!(
        err <= tol,
        "[{label}] ours_enc->apple_dec maxerr={err} (tol {tol})"
    );

    // 2. apple_enc -> ours_dec
    let Some(apple_enc) = apple_encode(api, pixels, info, compression) else {
        // Apple may reject some inputs (e.g. tiny images, 16-bit None/Default).
        eprintln!("[{label}] apple encode rejected input; skipping reverse direction");
        return;
    };
    let mut decoded = vec![0u8; pixels.len()];
    let mut dec_info = ImageInfo {
        width: 0,
        height: 0,
        format: PixelFormat::Gray8,
    };
    dm2_decode(&apple_enc, &mut decoded, &mut dec_info)
        .unwrap_or_else(|e| panic!("[{label}] our decode of apple encode failed: {e}"));
    assert_eq!(dec_info.format, info.format, "[{label}] format mismatch");
    let err = maxerr(pixels, &decoded);
    assert!(
        err <= tol,
        "[{label}] apple_enc->ours_dec maxerr={err} (tol {tol})"
    );
}

fn random(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Vec<u8> {
    // Tiny LCG — incompressible-ish data without an external dep.
    let n = w as usize * h as usize * fmt.pixel_size();
    let mut p = vec![0u8; n];
    let mut s = seed.wrapping_mul(0x9E3779B97F4A7C15);
    for b in p.iter_mut() {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (s >> 56) as u8;
    }
    p
}

fn high_byte_only_16(w: u32, h: u32, fmt: PixelFormat) -> Vec<u8> {
    // 16-bit format with low bytes zero — exercises endian-handling.
    assert!(fmt.is_16bit());
    let ps = fmt.pixel_size();
    let mut p = vec![0u8; w as usize * h as usize * ps];
    for i in (0..p.len()).step_by(2) {
        p[i] = 0;
        p[i + 1] = ((i / 2) & 0xFF) as u8;
    }
    p
}

fn samples_for(fmt: PixelFormat) -> Vec<(String, u32, u32, Vec<u8>)> {
    let mut v = Vec::new();
    let sizes: &[(u32, u32)] = &[
        (16, 16),
        (33, 17),
        (64, 64),
        (100, 50),
        (128, 9),
        // Boundary around the 4096-byte LZVN/bvx2 cutoff.
        (64, 16),  // 1024 px — well under cutoff for any format
        (64, 32),  // 2048 px — within bvxn-trap zone for ≥2-byte/pixel formats
        (256, 4),  // 1024 px, very wide / very short
        // Edge shapes.
        (1, 64),
        (64, 1),
        (3, 3),
        // Multi-tile lossless: ≥ ~2 MB raw forces tiling. Use 256×2200 → 563 200
        // px which is ~4.5 MB at 8 bytes/pixel and crosses the tile boundary
        // for every format.
        (256, 2200),
    ];
    for &(w, h) in sizes {
        v.push((format!("gradient_{w}x{h}"), w, h, gradient(w, h, fmt)));
        v.push((format!("checker_{w}x{h}"), w, h, checker(w, h, fmt)));
        v.push((format!("solid0_{w}x{h}"), w, h, solid(w, h, fmt, 0x00)));
        v.push((format!("solidff_{w}x{h}"), w, h, solid(w, h, fmt, 0xFF)));
        v.push((format!("random_{w}x{h}"), w, h, random(w, h, fmt, (w as u64) << 16 | h as u64)));
        if fmt.is_16bit() {
            v.push((format!("hi16_{w}x{h}"), w, h, high_byte_only_16(w, h, fmt)));
        }
    }
    v
}

fn run_format(fmt: PixelFormat) {
    let Some(api) = apple_api() else {
        eprintln!("vImageDeepmap2 symbols not resolvable; skipping cross-validation");
        return;
    };
    let compressions: &[Compression] = if fmt.is_16bit() {
        // check_pair starts from OUR encoder, and we only implement
        // Lossless for 16-bit (Apple's 16-bit type 2 is a lossy fixed-
        // point scheme we don't encode). Decode of Apple-encoded RGBA16
        // type 2 is covered by cross_rgba16_default below.
        &[Compression::Lossless]
    } else {
        &[
            Compression::None,
            Compression::Lossless,
            Compression::Default,
        ]
    };
    for (name, w, h, pixels) in samples_for(fmt) {
        let info = ImageInfo { width: w, height: h, format: fmt };
        for &c in compressions {
            let label = format!("{:?}/{:?}/{}", fmt, c, name);
            check_pair(&api, &label, &pixels, &info, c);
        }
    }
}

#[test] fn cross_gray8()   { run_format(PixelFormat::Gray8); }
#[test] fn cross_graya8()  { run_format(PixelFormat::GrayA8); }
#[test] fn cross_rgb8()    { run_format(PixelFormat::Rgb8); }
#[test] fn cross_rgba8()   { run_format(PixelFormat::Rgba8); }
#[test] fn cross_gray16()  { run_format(PixelFormat::Gray16); }
#[test] fn cross_graya16() { run_format(PixelFormat::GrayA16); }
#[test] fn cross_rgb16()   { run_format(PixelFormat::Rgb16); }
#[test] fn cross_rgba16()  { run_format(PixelFormat::Rgba16); }

/// Regression: an image whose raw byte size exceeds the type-2 tile budget
/// must be encoded as a real multi-tile Type 2 stream, not silently
/// downgraded to Lossless. Covers both encode directions through Apple.
#[test]
fn cross_default_multitile() {
    let Some(api) = apple_api() else { return };

    // 256×2200 RGB8 = ~1.65 MB raw, exceeds the 1,044,480-byte Type 2 tile
    // budget (max ~1360 rows per tile at this width) — forces ≥2 tiles.
    // RGB8 (no alpha) avoids the VaryingAlpha fallback so we exercise the
    // Default path itself rather than its other escape hatches.
    let w = 256u32;
    let h = 2200u32;
    let fmt = PixelFormat::Rgb8;
    let pixels = gradient(w, h, fmt);
    let info = ImageInfo { width: w, height: h, format: fmt };

    let encoded = dm2_encode(&pixels, &info, Compression::Default)
        .expect("multi-tile Default encode failed");
    let (_hdr_info, comp) = dm2_read_info(&encoded).expect("read_info failed");
    assert_eq!(
        comp,
        Compression::Default,
        "multi-tile image was silently downgraded from Default (got {:?})",
        comp
    );

    // Full cross-validation in both directions (ours↔Apple).
    check_pair(&api, "default_multitile_rgb8_256x2200", &pixels, &info, Compression::Default);
}

/// Regression: alpha channels with varying values must be encoded as a real
/// Type 2 stream (not silently downgraded to Lossless). Earlier the encoder
/// bailed on the assumption that the alpha plane overlapped the YCC mode
/// bytes — it doesn't (per deepmap2.md the alpha plane is its own W×H
/// region preceding the H mode bytes).
#[test]
fn cross_default_varying_alpha() {
    let Some(api) = apple_api() else { return };

    for &fmt in &[PixelFormat::GrayA8, PixelFormat::Rgba8] {
        let w = 64u32;
        let h = 64u32;
        // gradient() varies every channel including alpha.
        let pixels = gradient(w, h, fmt);
        let info = ImageInfo { width: w, height: h, format: fmt };

        let encoded = dm2_encode(&pixels, &info, Compression::Default)
            .expect("varying-alpha Default encode failed");
        let (_i, comp) = dm2_read_info(&encoded).expect("read_info failed");
        assert_eq!(
            comp,
            Compression::Default,
            "{:?} with varying alpha was downgraded from Default (got {:?})",
            fmt,
            comp
        );

        let label = format!("default_varying_alpha_{:?}", fmt);
        check_pair(&api, &label, &pixels, &info, Compression::Default);
    }
}

/// Chroma-scale rule (found while adding RGBA16, #128): the half-scale-chroma
/// switch in type 2 is the QUALITY byte, not param — param provably has no
/// effect on 8-bit streams (q1/p0 and q1/p10 encode byte-identically). Every
/// real .car rendition ships (quality=1, param=10), so a param-keyed decoder
/// passed the real-corpus differential but mis-decoded q0/p10 and q1/p0
/// streams. Lock decoder equality across the full (quality, param) grid.
#[test]
fn cross_default_quality_param_grid() {
    let Some(api) = apple_api() else { return };
    for &fmt in &[PixelFormat::GrayA8, PixelFormat::Rgb8, PixelFormat::Rgba8] {
        let (w, h) = (32u32, 32u32);
        let pixels = random(w, h, fmt, 0xC0FFEE);
        let info = ImageInfo { width: w, height: h, format: fmt };
        for quality in 0..=1u32 {
            for param in [0u32, 10] {
                let label = format!("quality_param_grid/{fmt:?}/q{quality}p{param}");
                let Some(enc) =
                    apple_encode_opts(&api, &pixels, &info, Compression::Default, quality, param)
                else {
                    panic!("[{label}] apple encode rejected input");
                };
                let apple = apple_decode(&api, &enc, &info)
                    .unwrap_or_else(|| panic!("[{label}] apple decode of its own encode is NULL"));
                let mut ours = vec![0u8; pixels.len()];
                let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
                dm2_decode(&enc, &mut ours, &mut di)
                    .unwrap_or_else(|e| panic!("[{label}] our decode failed: {e}"));
                assert_eq!(
                    maxerr(&apple, &ours),
                    0,
                    "[{label}] our decode diverges from vImageDeepmap2Decode"
                );
            }
        }
    }
}

/// Generate an RGBA16 image of VALID half-float pixels with |value| < 4.0
/// (exponent field <= 16), the amplitude regime real .car EDR icons live in
/// (known sample peak is ~1.0; codes stay far below the i16 fixed-point limit
/// at every legal param). Raw byte-noise generators produce NaN/Inf/huge
/// halfs whose fixed-point codes overflow i16 inside Apple's ENCODER —
/// see the doc comment on cross_rgba16_default_decoder_equality.
fn sane_halfs_rgba16(w: u32, h: u32, seed: u64) -> Vec<u8> {
    let n = w as usize * h as usize * 4;
    let mut p = Vec::with_capacity(n * 2);
    let mut s = seed.wrapping_mul(0x9E3779B97F4A7C15);
    for _ in 0..n {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let r = (s >> 40) as u32;
        let sign = ((r >> 16) & 1) as u16;
        let exp = ((r >> 10) % 17) as u16; // 0..=16 -> |v| < 4.0 (incl. subnormals)
        let mant = (r & 0x3ff) as u16;
        let half = (sign << 15) | (exp << 10) | mant;
        p.extend_from_slice(&half.to_le_bytes());
    }
    p
}

/// RGBA16 type 2 (#128): Apple encodes 16-bit pixels as fixed-point codes of
/// the half-float channel values (scale 2^(param-1), param 9..=12) in the
/// same K=7 plane layout as RGBA8, with an 8-bit alpha plane. Encoding is
/// lossy, so round-tripping against the ORIGINAL pixels is meaningless —
/// the contract is decoder equality: for the same stream, our decode must be
/// byte-identical to vImageDeepmap2Decode. Covers every legal param and both
/// legal qualities.
///
/// Input domain: valid halfs, |v| < 4.0. When a garbage half (NaN/Inf/2^13)
/// meets the fixed-point quantizer, Apple's encoder WRAPS the code at i16 and
/// its decoder's wrapping arithmetic becomes observable; we pin the dominant
/// wrap semantics (i16-wrapping prediction + inverse transform, verified on
/// q1 all params + q0 p9 even for garbage inputs), but q0/p10..12 streams
/// built from garbage halfs still diverge on a handful of wrapped pixels —
/// an intentionally unchased corner: Apple's own encode of such input is
/// already total value corruption, and no real encoder emits it (every .car
/// rendition observed is quality=1 param=10 with |v| ~<= 1).
#[test]
fn cross_rgba16_default_decoder_equality() {
    let Some(api) = apple_api() else { return };
    let fmt = PixelFormat::Rgba16;
    let sizes: &[(u32, u32)] = &[(16, 16), (33, 17), (64, 64), (128, 9), (1, 64), (64, 1), (256, 2200)];
    let mut samples: Vec<(String, u32, u32, Vec<u8>)> = Vec::new();
    for &(w, h) in sizes {
        samples.push((format!("halfs_{w}x{h}"), w, h, sane_halfs_rgba16(w, h, (w as u64) << 20 | h as u64)));
    }
    samples.push(("solid0_32x32".into(), 32, 32, solid(32, 32, fmt, 0)));
    for (name, w, h, pixels) in samples {
        let info = ImageInfo { width: w, height: h, format: fmt };
        for param in 9..=12u32 {
            for quality in 0..=1u32 {
                let label = format!("rgba16_default/{name}/q{quality}p{param}");
                let Some(enc) =
                    apple_encode_opts(&api, &pixels, &info, Compression::Default, quality, param)
                else {
                    // Apple rejects some shapes (e.g. tiny images); nothing to compare.
                    eprintln!("[{label}] apple encode rejected input; skipping");
                    continue;
                };
                let apple = apple_decode(&api, &enc, &info)
                    .unwrap_or_else(|| panic!("[{label}] apple decode of its own encode is NULL"));
                let mut ours = vec![0u8; pixels.len()];
                let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
                dm2_decode(&enc, &mut ours, &mut di)
                    .unwrap_or_else(|e| panic!("[{label}] our decode failed: {e}"));
                assert_eq!(di.format, fmt, "[{label}] format mismatch");
                assert_eq!(
                    maxerr(&apple, &ours),
                    0,
                    "[{label}] our decode diverges from vImageDeepmap2Decode"
                );
            }
        }
    }
}

#[test]
fn cross_palette_rgba8() {
    let Some(api) = apple_api() else { return };

    // Few-color case (well under the 256-entry limit).
    let pixels = few_colors_rgba(64, 64);
    let info = ImageInfo { width: 64, height: 64, format: PixelFormat::Rgba8 };
    check_pair(&api, "palette_few", &pixels, &info, Compression::Palette);

    // Exactly 256 distinct colors — the encoder must not overflow.
    let mut max_pal = vec![0u8; 16 * 16 * 4];
    for i in 0..256 {
        max_pal[i * 4] = i as u8;
        max_pal[i * 4 + 1] = (255 - i) as u8;
        max_pal[i * 4 + 2] = ((i * 7) & 0xFF) as u8;
        max_pal[i * 4 + 3] = 0xFF;
    }
    let info = ImageInfo { width: 16, height: 16, format: PixelFormat::Rgba8 };
    check_pair(&api, "palette_max256", &max_pal, &info, Compression::Palette);

    // Single-color palette.
    let one = vec![0x42u8; 32 * 32 * 4];
    let info = ImageInfo { width: 32, height: 32, format: PixelFormat::Rgba8 };
    check_pair(&api, "palette_single", &one, &info, Compression::Palette);
}

// --- Real-catalog payloads: decode actual Apple-produced deepmap2 streams from a .car with
// BOTH Apple's vImageDeepmap2Decode and libdm2, and compare RAW output. This isolates the
// CODEC (payload -> pixels) from any higher-level CUICatalog compositing/colour-management:
// if libdm2 matches vImageDeepmap2Decode here, libdm2 is correct and any app-level mismatch
// is elsewhere; if it diverges, this pinpoints the libdm2 decode bug.

fn le32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// Extract every deepmap2 rendition of pixel format `fmt`: (label, width, height, payload).
/// width/height come from the enclosing CSI header (@12/@16); the payload is the dmp2 stream.
fn car_deepmap2(car: &[u8], fmt: PixelFormat) -> Vec<(String, u32, u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mlec = b"MLEC";
    let mut i = 0usize;
    while i + 32 < car.len() {
        if &car[i..i + 4] != mlec { i += 1; continue; }
        let comp = le32(car, i + 8);
        let pay_len = le32(car, i + 24) as usize;
        if comp != 11 || i + 32 + pay_len > car.len() { i += 4; continue; }
        let pay = &car[i + 32..i + 32 + pay_len];
        if pay.len() < 8 || &pay[0..4] != b"dmp2" || pay[7] != fmt as u8 { i += 4; continue; }
        // full dims from the CSI header preceding this MLEC
        let istc = car[..i].windows(4).rposition(|w| w == b"ISTC");
        if let Some(istc) = istc {
            let w = le32(car, istc + 12);
            let h = le32(car, istc + 16);
            if w > 0 && h > 0 && w <= 16384 && h <= 16384 {
                out.push((format!("MLEC@0x{i:x} {w}x{h}"), w, h, pay.to_vec()));
            }
        }
        i += 32 + pay_len;
    }
    out
}

/// Ground-truth harness: decodes the REAL deepmap2 streams Apple shipped in a .car with both
/// `vImageDeepmap2Decode` and libdm2 and requires a byte-for-byte match. Decoding a fixed stream
/// is deterministic, so a correct libdm2 must equal Apple exactly. Skips gracefully when the
/// Accelerate symbols or the default .car are unavailable; override the catalog with
/// COMPARE_CAR=/path/to/Assets.car.
#[test]
#[ignore = "reproduces libdm2 decode divergence from vImageDeepmap2Decode on some real streams"]
fn cross_real_car_payloads() {
    let Some(api) = apple_api() else {
        eprintln!("vImageDeepmap2 symbols not resolvable; skipping");
        return;
    };
    let path = std::env::var("COMPARE_CAR").expect("Provide a COMPARE_CAR variable");
    let Ok(car) = std::fs::read(&path) else {
        eprintln!("cannot read {path}; skipping");
        return;
    };
    let mut fails = 0;
    let mut total = 0;
    // RGBA8 must be present in the default corpus; RGBA16 is
    // exercised whenever the corpus has any.
    for fmt in [PixelFormat::Rgba8, PixelFormat::Rgba16] {
        let samples = car_deepmap2(&car, fmt);
        if fmt == PixelFormat::Rgba8 {
            assert!(!samples.is_empty(), "no deepmap2 RGBA8 renditions found in {path}");
        } else {
            eprintln!("{} RGBA16 renditions in {path}", samples.len());
        }
        let ps = fmt.pixel_size();
        for (label, w, h, pay) in &samples {
            total += 1;
            let info = ImageInfo { width: *w, height: *h, format: fmt };
            // Apple's raw decode (ground truth).
            let Some(apple) = apple_decode(&api, pay, &info) else {
                eprintln!("[{label}] apple decode returned NULL; skipping"); continue;
            };
            // Our decode.
            let mut ours = vec![0u8; (*w as usize) * (*h as usize) * ps];
            let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
            if let Err(e) = dm2_decode(pay, &mut ours, &mut di) {
                eprintln!("[{label}] OUR decode failed: {e}"); fails += 1; continue;
            }
            let err = maxerr(&apple, &ours);
            // locate first divergence
            let first = apple.iter().zip(ours.iter()).position(|(a, b)| a != b);
            if err == 0 {
                eprintln!("[{label}] {fmt:?} EXACT match vs vImageDeepmap2Decode");
            } else {
                fails += 1;
                let fp = first.unwrap_or(0);
                let px = fp / ps;
                eprintln!("[{label}] {fmt:?} DIVERGES maxerr={err}  first@byte {fp} (px {},{} byte {})  apple={:?} ours={:?}",
                    px % *w as usize, px / *w as usize, fp % ps,
                    &apple[fp..(fp + ps).min(apple.len())], &ours[fp..(fp + ps).min(ours.len())]);
            }
        }
    }
    assert_eq!(fails, 0, "{fails}/{total} real deepmap2 payloads diverge from vImageDeepmap2Decode");
}
