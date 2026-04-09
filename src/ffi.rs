use crate::error::Dm2Error;
use crate::format::{Compression, ImageInfo, PixelFormat};
use std::ffi::CStr;
use std::path::Path;
use std::slice;

#[repr(C)]
pub struct Dm2ImageInfo {
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

impl From<&Dm2ImageInfo> for ImageInfo {
    fn from(c: &Dm2ImageInfo) -> Self {
        ImageInfo {
            width: c.width,
            height: c.height,
            format: PixelFormat::from_u8(c.format as u8).unwrap_or(PixelFormat::Rgba8),
        }
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
    let info_c = &*info;
    let info_r = ImageInfo::from(info_c);
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
    let mut info_r = ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };

    let r = crate::dm2_decode(data, pixels, &mut info_r);
    *info = Dm2ImageInfo::from(&info_r);
    result_to_code(r)
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
    match crate::dm2_read_info(data) {
        Ok((info_r, _)) => {
            *info = Dm2ImageInfo::from(&info_r);
            0
        }
        Err(e) => e as i32,
    }
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
    let info_r = ImageInfo::from(&*info);
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
    let info_c = &*info;
    let info_r = ImageInfo::from(info_c);
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
