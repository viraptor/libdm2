pub mod color;
pub mod decode;
pub mod encode;
pub mod error;
pub mod ffi;
pub mod format;
pub mod lzfse;
pub mod lzvn;
pub mod predict;
pub mod verified;
pub mod verified_lzvn;

pub use error::{Dm2Error, Result};
pub use format::{Compression, ImageInfo, PixelFormat};

/// Encode pixel data into deepmap2 format.
/// With `compression = None`, pass a specific type. For best compression, use `encode_auto`.
pub fn dm2_encode(pixels: &[u8], info: &ImageInfo, compression: Compression) -> Result<Vec<u8>> {
    encode::encode(pixels, info, compression)
}

/// Encode with automatic compression selection (tries all methods, picks smallest).
pub fn dm2_encode_auto(pixels: &[u8], info: &ImageInfo) -> Result<Vec<u8>> {
    encode::encode_auto(pixels, info)
}

/// Decode deepmap2 data into pixel buffer.
/// `info` is filled with the image dimensions and format.
pub fn dm2_decode(data: &[u8], pixels: &mut [u8], info: &mut ImageInfo) -> Result<()> {
    decode::decode(data, pixels, info)
}

/// Read image info from deepmap2 header without decoding pixels.
pub fn dm2_read_info(data: &[u8]) -> Result<(ImageInfo, Compression)> {
    decode::read_info(data)
}

/// Bytes per pixel for a given format.
pub fn dm2_pixel_size(format: PixelFormat) -> usize {
    format.pixel_size()
}

/// Upper bound on encoded output size (for pre-allocating buffers).
pub fn dm2_encode_bound(info: &ImageInfo) -> usize {
    // Worst case: type 2 expands ~2x, LZFSE can expand ~12.5%, plus header + tile overhead
    let raw = info.raw_size();
    raw * 3 + 4096
}
