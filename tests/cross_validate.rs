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
        quality: 0,
        param: if info.format.is_16bit() { 12 } else { 0 },
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
        // Per deepmap2.md, only Lossless works correctly for 16-bit.
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

/// Extract every deepmap2 RGBA8 rendition: (label, width, height, payload).
/// width/height come from the enclosing CSI header (@12/@16); the payload is the dmp2 stream.
fn car_deepmap2_rgba8(car: &[u8]) -> Vec<(String, u32, u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mlec = b"MLEC";
    let mut i = 0usize;
    while i + 32 < car.len() {
        if &car[i..i + 4] != mlec { i += 1; continue; }
        let comp = le32(car, i + 8);
        let pay_len = le32(car, i + 24) as usize;
        if comp != 11 || i + 32 + pay_len > car.len() { i += 4; continue; }
        let pay = &car[i + 32..i + 32 + pay_len];
        if pay.len() < 8 || &pay[0..4] != b"dmp2" || pay[7] != 4 /* RGBA8 */ { i += 4; continue; }
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

/// #128 reproduction / ground-truth harness. Decodes the REAL deepmap2 streams Apple shipped in
/// a .car with both `vImageDeepmap2Decode` and libdm2, comparing raw RGBA. Decoding a fixed stream
/// is deterministic, so a correct libdm2 must match Apple byte-for-byte. Currently it does NOT for
/// some renditions (Δ up to ~91, accumulating in predicted runs) — a libdm2 Default/type-2
/// reconstruction bug; ruled out: YCoCg /2-vs->>1, and dropping/None-gating the negative-residual
/// adjustment (all leave the divergence unchanged or worse).
/// `#[ignore]`d because it depends on a host .car path and tracks the open #128 gap; run on demand:
///   cargo test --release -- --ignored cross_real_car_payloads --nocapture
/// When #128 is fixed this goes green and the attribute should be removed.
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
    let samples = car_deepmap2_rgba8(&car);
    assert!(!samples.is_empty(), "no deepmap2 RGBA8 renditions found in {path}");

    let mut fails = 0;
    for (label, w, h, pay) in &samples {
        let info = ImageInfo { width: *w, height: *h, format: PixelFormat::Rgba8 };
        // Apple's raw decode (ground truth).
        let Some(apple) = apple_decode(&api, pay, &info) else {
            eprintln!("[{label}] apple decode returned NULL; skipping"); continue;
        };
        // Our decode.
        let mut ours = vec![0u8; (*w as usize) * (*h as usize) * 4];
        let mut di = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        if let Err(e) = dm2_decode(pay, &mut ours, &mut di) {
            eprintln!("[{label}] OUR decode failed: {e}"); fails += 1; continue;
        }
        let err = maxerr(&apple, &ours);
        // locate first divergence
        let first = apple.iter().zip(ours.iter()).position(|(a, b)| a != b);
        if err == 0 {
            eprintln!("[{label}] EXACT match vs vImageDeepmap2Decode");
        } else {
            fails += 1;
            let fp = first.unwrap_or(0);
            let px = fp / 4;
            eprintln!("[{label}] DIVERGES maxerr={err}  first@byte {fp} (px {},{} chan {})  apple={:?} ours={:?}",
                px % *w as usize, px / *w as usize, fp % 4,
                &apple[fp..(fp + 4).min(apple.len())], &ours[fp..(fp + 4).min(ours.len())]);
        }
    }
    assert_eq!(fails, 0, "{fails}/{} real deepmap2 payloads diverge from vImageDeepmap2Decode", samples.len());
}
