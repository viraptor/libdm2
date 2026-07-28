use crate::error::Dm2Error;
use crate::format::{Compression, ImageInfo, PixelFormat};
use std::ffi::CStr;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::slice;
use std::sync::Once;

static HOOK: Once = Once::new();

/// Install a lightweight panic hook exactly once. The DEFAULT Rust hook captures
/// and symbolizes a backtrace (gimli/addr2line DWARF line-table parsing across every
/// loaded image). So we print just the message+location (no
/// backtrace) and rely on the `guard()` catch_unwind below to turn the panic into a
/// clean error code. This keeps a malformed/unsupported swatch from hanging the host.
fn install_hook() {
    HOOK.call_once(|| {
        panic::set_hook(Box::new(|pi| {
            let loc = pi
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "<unknown>".into());
            let msg = pi
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| pi.payload().downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("<non-string panic payload>");
            eprintln!("[libdm2] panic at {loc}: {msg}");
        }));
    });
}

/// Run an FFI body, converting any panic into `Dm2Error::DecodeFailed` instead of
/// unwinding across the C boundary. Instant (no backtrace) thanks to `install_hook`.
fn guard<F: FnOnce() -> i32>(f: F) -> i32 {
    install_hook();
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => Dm2Error::DecodeFailed as i32,
    }
}

#[repr(C)]
pub struct Dm2ImageInfo {
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

impl TryFrom<&Dm2ImageInfo> for ImageInfo {
    type Error = Dm2Error;
    fn try_from(c: &Dm2ImageInfo) -> Result<Self, Dm2Error> {
        let format = PixelFormat::from_u8(c.format as u8)?;
        Ok(ImageInfo { width: c.width, height: c.height, format })
    }
}

impl From<&ImageInfo> for Dm2ImageInfo {
    fn from(info: &ImageInfo) -> Self {
        Dm2ImageInfo {
            width: info.width,
            height: info.height,
            format: info.format as u32,
        }
    }
}

fn compression_from_u32(v: u32) -> Option<Compression> {
    match v {
        1 => Some(Compression::None),
        2 => Some(Compression::Default),
        3 => Some(Compression::Lossless),
        4 => Some(Compression::Palette),
        _ => None,
    }
}

fn result_to_code(r: Result<(), Dm2Error>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => e as i32,
    }
}

/// Encode pixels into deepmap2 format. Output is malloc'd; caller must free with `dm2_free`.
/// `compression`: 0 = auto, 1-4 = specific type.
/// On success, `*out` and `*out_len` are set. Returns 0 on success, negative on error.
#[no_mangle]
pub unsafe extern "C" fn dm2_encode(
    pixels: *const u8, pixels_len: usize,
    info: *const Dm2ImageInfo,
    compression: u32,
    out: *mut *mut u8, out_len: *mut usize,
) -> i32 {
    if pixels.is_null() || info.is_null() || out.is_null() || out_len.is_null() {
        return Dm2Error::InvalidArg as i32;
    }
    let info_r = match ImageInfo::try_from(&*info) {
        Ok(i) => i,
        Err(e) => return e as i32,
    };
    let pix = slice::from_raw_parts(pixels, pixels_len);

    let result = if compression == 0 {
        crate::dm2_encode_auto(pix, &info_r)
    } else {
        let Some(comp) = compression_from_u32(compression) else {
            return Dm2Error::InvalidArg as i32;
        };
        crate::dm2_encode(pix, &info_r, comp)
    };

    match result {
        Ok(encoded) => {
            let len = encoded.len();
            let ptr = encoded.leak().as_mut_ptr();
            *out = ptr;
            *out_len = len;
            0
        }
        Err(e) => e as i32,
    }
}

/// Decode deepmap2 data into a pixel buffer.
/// `info` is filled with width/height/format on success.
#[no_mangle]
pub unsafe extern "C" fn dm2_decode(
    data: *const u8, data_len: usize,
    pixels: *mut u8, pixels_len: usize,
    info: *mut Dm2ImageInfo,
) -> i32 {
    if data.is_null() || pixels.is_null() || info.is_null() {
        return Dm2Error::InvalidArg as i32;
    }
    let data = slice::from_raw_parts(data, data_len);
    let pixels = slice::from_raw_parts_mut(pixels, pixels_len);

    guard(move || {
        let mut info_r = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };
        let r = crate::dm2_decode(data, pixels, &mut info_r);
        // SAFETY: `info` non-null checked above; write occurs inside the guard so a
        // panic in dm2_decode still lands here as DecodeFailed rather than unwinding.
        unsafe { *info = Dm2ImageInfo::from(&info_r); }
        result_to_code(r)
    })
}

/// Read image info without decoding.
#[no_mangle]
pub unsafe extern "C" fn dm2_read_info(
    data: *const u8, data_len: usize,
    info: *mut Dm2ImageInfo,
) -> i32 {
    if data.is_null() || info.is_null() {
        return Dm2Error::InvalidArg as i32;
    }
    let data = slice::from_raw_parts(data, data_len);
    guard(move || match crate::dm2_read_info(data) {
        Ok((info_r, _)) => {
            // SAFETY: `info` non-null checked above; guarded against panic.
            unsafe { *info = Dm2ImageInfo::from(&info_r); }
            0
        }
        Err(e) => e as i32,
    })
}

/// Returns bytes per pixel for a format, or 0 if invalid.
#[no_mangle]
pub extern "C" fn dm2_pixel_size(format: u32) -> u32 {
    PixelFormat::from_u8(format as u8)
        .map(|f| f.pixel_size() as u32)
        .unwrap_or(0)
}

/// Upper bound on encoded output size.
#[no_mangle]
pub unsafe extern "C" fn dm2_encode_bound(info: *const Dm2ImageInfo) -> usize {
    if info.is_null() { return 0; }
    let Ok(info_r) = ImageInfo::try_from(&*info) else { return 0; };
    crate::dm2_encode_bound(&info_r)
}

/// Free a buffer allocated by `dm2_encode`.
#[no_mangle]
pub unsafe extern "C" fn dm2_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}

// --- File convenience wrappers ---

/// Encode pixels and write to a file. Returns 0 on success.
#[no_mangle]
pub unsafe extern "C" fn dm2_encode_file(
    pixels: *const u8, pixels_len: usize,
    info: *const Dm2ImageInfo,
    compression: u32,
    path: *const std::ffi::c_char,
) -> i32 {
    if path.is_null() || pixels.is_null() || info.is_null() {
        return Dm2Error::InvalidArg as i32;
    }
    let path = match CStr::from_ptr(path).to_str() {
        Ok(s) => Path::new(s),
        Err(_) => return Dm2Error::InvalidArg as i32,
    };
    let info_r = match ImageInfo::try_from(&*info) {
        Ok(i) => i,
        Err(e) => return e as i32,
    };
    let pix = slice::from_raw_parts(pixels, pixels_len);

    let result = if compression == 0 {
        crate::dm2_encode_auto(pix, &info_r)
    } else {
        let Some(comp) = compression_from_u32(compression) else {
            return Dm2Error::InvalidArg as i32;
        };
        crate::dm2_encode(pix, &info_r, comp)
    };

    match result {
        Ok(encoded) => match std::fs::write(path, &encoded) {
            Ok(()) => 0,
            Err(_) => Dm2Error::IoError as i32,
        },
        Err(e) => e as i32,
    }
}

/// Read a deepmap2 file and decode into a pixel buffer.
/// `info` is filled on success.
#[no_mangle]
pub unsafe extern "C" fn dm2_decode_file(
    path: *const std::ffi::c_char,
    pixels: *mut u8, pixels_len: usize,
    info: *mut Dm2ImageInfo,
) -> i32 {
    if path.is_null() || pixels.is_null() || info.is_null() {
        return Dm2Error::InvalidArg as i32;
    }
    let path = match CStr::from_ptr(path).to_str() {
        Ok(s) => Path::new(s),
        Err(_) => return Dm2Error::InvalidArg as i32,
    };

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return Dm2Error::IoError as i32,
    };

    dm2_decode(data.as_ptr(), data.len(), pixels, pixels_len, info)
}
