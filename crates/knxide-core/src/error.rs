//! Errors shared by the frame codecs.

use core::fmt;

/// A frame or field could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended before the structure was complete.
    UnexpectedEnd,
    /// A field holds a value the spec does not allow. The string names the field.
    Invalid(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("unexpected end of input"),
            Self::Invalid(field) => write!(f, "invalid {field}"),
        }
    }
}

impl core::error::Error for DecodeError {}

/// A value cannot be represented on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// A field or the whole structure is too long for its length field.
    TooLong(&'static str),
    /// A field holds a value outside its wire range. The string names the field.
    OutOfRange(&'static str),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong(field) => write!(f, "{field} too long"),
            Self::OutOfRange(field) => write!(f, "{field} out of range"),
        }
    }
}

impl core::error::Error for EncodeError {}
