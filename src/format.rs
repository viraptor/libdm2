use crate::error::{Dm2Error, Result};

pub const MAGIC: [u8; 4] = [0x64, 0x6d, 0x70, 0x32]; // "dmp2"

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PixelFormat {
    Gray8 = 1,
    GrayA8 = 2,
    Rgb8 = 3,
    Rgba8 = 4,
    Gray16 = 0x11,
    GrayA16 = 0x12,
    Rgb16 = 0x13,
    Rgba16 = 0x14,
}

impl PixelFormat {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(Self::Gray8),
            2 => Ok(Self::GrayA8),
            3 => Ok(Self::Rgb8),
            4 => Ok(Self::Rgba8),
            0x11 => Ok(Self::Gray16),
            0x12 => Ok(Self::GrayA16),
            0x13 => Ok(Self::Rgb16),
            0x14 => Ok(Self::Rgba16),
            _ => Err(Dm2Error::BadFormat),
        }
    }

    pub fn pixel_size(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::GrayA8 => 2,
            Self::Rgb8 => 3,
            Self::Rgba8 => 4,
            Self::Gray16 => 2,
            Self::GrayA16 => 4,
            Self::Rgb16 => 6,
            Self::Rgba16 => 8,
        }
    }

    pub fn channels(self) -> usize {
        match self {
            Self::Gray8 | Self::Gray16 => 1,
            Self::GrayA8 | Self::GrayA16 => 2,
            Self::Rgb8 | Self::Rgb16 => 3,
            Self::Rgba8 | Self::Rgba16 => 4,
        }
    }

    pub fn is_16bit(self) -> bool {
        (self as u8) >= 0x11
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Compression {
    None = 1,
    Default = 2,
    Lossless = 3,
    Palette = 4,
}

impl Compression {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(Self::None),
            2 => Ok(Self::Default),
            3 => Ok(Self::Lossless),
            4 => Ok(Self::Palette),
            _ => Err(Dm2Error::BadFormat),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

impl ImageInfo {
    /// Total raw pixel-data size in bytes, or `Err(BufferTooSmall)` if
    /// `width * height * pixel_size` would overflow `usize`. Use this
    /// before allocating buffers sized from untrusted header dimensions.
    pub fn checked_raw_size(&self) -> Result<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|wh| wh.checked_mul(self.format.pixel_size()))
            .ok_or(Dm2Error::BufferTooSmall)
    }

    /// Infallible version of [`Self::checked_raw_size`]. Panics on
    /// `usize` overflow — only safe to call after the dimensions have
    /// already been validated (e.g. on info you constructed yourself).
    pub fn raw_size(&self) -> usize {
        self.checked_raw_size().expect("raw_size overflow")
    }

    pub fn row_bytes(&self) -> usize {
        self.width as usize * self.format.pixel_size()
    }
}

#[derive(Debug, Clone)]
pub struct Header {
    pub compression: Compression,
    pub quality: u8,
    pub param: u8,
    pub format: PixelFormat,
    pub tile_width: u16,
    pub tile_height: u16,
    pub palette: Option<Vec<[u8; 4]>>,
    pub palette_bpe: u8,
}

impl Header {
    pub fn read(data: &[u8]) -> Result<(Self, usize)> {
        if data.len() < 12 {
            return Err(Dm2Error::BufferTooSmall);
        }
        if data[0..4] != MAGIC {
            return Err(Dm2Error::BadMagic);
        }
        let compression = Compression::from_u8(data[4])?;
        let quality = data[5];
        let param = data[6];
        let format = PixelFormat::from_u8(data[7])?;
        let tile_width = u16::from_le_bytes([data[8], data[9]]);
        let tile_height = u16::from_le_bytes([data[10], data[11]]);

        let mut consumed = 12;
        let mut palette = None;
        let mut palette_bpe = 4u8;

        if compression == Compression::Palette && format == PixelFormat::Rgba8 {
            if data.len() < 16 {
                return Err(Dm2Error::BufferTooSmall);
            }
            let count = u16::from_le_bytes([data[12], data[13]]) as usize;
            let bpe = u16::from_le_bytes([data[14], data[15]]) as usize;
            consumed = 16;
            if bpe != 3 && bpe != 4 {
                return Err(Dm2Error::BadFormat);
            }
            let palette_bytes = count * 4;
            if data.len() < consumed + palette_bytes {
                return Err(Dm2Error::BufferTooSmall);
            }
            let mut entries = Vec::with_capacity(count);
            for i in 0..count {
                let off = consumed + i * 4;
                entries.push([data[off], data[off + 1], data[off + 2], data[off + 3]]);
            }
            consumed += palette_bytes;
            palette = Some(entries);
            palette_bpe = bpe as u8;
        }

        Ok((
            Header {
                compression,
                quality,
                param,
                format,
                tile_width,
                tile_height,
                palette,
                palette_bpe,
            },
            consumed,
        ))
    }

    pub fn write(&self, buf: &mut [u8]) -> Result<usize> {
        let palette_bytes = self
            .palette
            .as_ref()
            .map(|p| 4 + p.len() * 4)
            .unwrap_or(0);
        let needed = 12 + palette_bytes;
        if buf.len() < needed {
            return Err(Dm2Error::BufferTooSmall);
        }
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = self.compression as u8;
        buf[5] = self.quality;
        buf[6] = self.param;
        buf[7] = self.format as u8;
        buf[8..10].copy_from_slice(&self.tile_width.to_le_bytes());
        buf[10..12].copy_from_slice(&self.tile_height.to_le_bytes());

        if let Some(palette) = &self.palette {
            let count = palette.len() as u16;
            buf[12..14].copy_from_slice(&count.to_le_bytes());
            buf[14..16].copy_from_slice(&4u16.to_le_bytes());
            for (i, entry) in palette.iter().enumerate() {
                let off = 16 + i * 4;
                buf[off..off + 4].copy_from_slice(entry);
            }
        }

        Ok(needed)
    }
}

/// Compute tile height for a given compression type and image dimensions.
/// Tiles are full-width horizontal strips capped by a raw data budget.
/// The budget math is Verus-verified in [`crate::verified::tile_rows_for_budget`]
/// (result in 1..=height and within budget unless one row alone exceeds it).
pub fn compute_tile_height(compression: Compression, width: u32, height: u32, pixel_size: usize) -> u32 {
    let budget: usize = match compression {
        Compression::None => return height, // no tiling
        Compression::Default | Compression::Palette => 1_044_480,
        Compression::Lossless => 2_097_152,
    };
    let row_bytes = width as usize * pixel_size;
    if row_bytes == 0 {
        return height;
    }
    if height == 0 {
        // Degenerate case preserved from the original implementation:
        // 1 if a single row alone exceeds the budget, else 0.
        return if budget / row_bytes == 0 { 1 } else { 0 };
    }
    crate::verified::tile_rows_for_budget(budget, row_bytes, height)
}
