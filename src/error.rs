use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Dm2Error {
    InvalidArg = -1,
    BadMagic = -2,
    BadFormat = -3,
    BufferTooSmall = -4,
    AllocFailed = -5,
    DecodeFailed = -6,
    EncodeFailed = -7,
    IoError = -8,
}

impl fmt::Display for Dm2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArg => write!(f, "invalid argument"),
            Self::BadMagic => write!(f, "bad magic (not a dmp2 file)"),
            Self::BadFormat => write!(f, "unsupported or mismatched pixel format"),
            Self::BufferTooSmall => write!(f, "output buffer too small"),
            Self::AllocFailed => write!(f, "memory allocation failed"),
            Self::DecodeFailed => write!(f, "decode failed"),
            Self::EncodeFailed => write!(f, "encode failed"),
            Self::IoError => write!(f, "I/O error"),
        }
    }
}

impl std::error::Error for Dm2Error {}

impl From<std::io::Error> for Dm2Error {
    fn from(_: std::io::Error) -> Self {
        Dm2Error::IoError
    }
}

pub type Result<T> = std::result::Result<T, Dm2Error>;
