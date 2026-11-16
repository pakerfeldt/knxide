//! Errors of the offline stages: reading product and project data, building the image.

use std::fmt;

/// Product or project data could not be read, or cannot be turned into a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The XML is malformed.
    Xml(String),
    /// A required element or attribute is absent. The string names it.
    Missing(String),
    /// An attribute or value cannot be interpreted.
    Invalid {
        /// What was being read.
        what: &'static str,
        /// The offending text, possibly shortened.
        value: String,
    },
    /// An identifier refers to something the product data does not define.
    UnknownReference(String),
    /// The mask version is not one this crate has rules for.
    UnsupportedMask(u16),
    /// The data uses a feature this crate has no rules for. The string names it.
    Unsupported(String),
    /// A communication table does not fit the space the product reserves for it.
    TableOverflow(String),
    /// A value does not fit, or a location lies outside its segment.
    OutOfRange(String),
    /// A project archive cannot be read. The string says why.
    Archive(String),
    /// The project is password-protected. Such projects are not opened.
    PasswordProtected,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xml(message) => write!(f, "malformed XML: {message}"),
            Self::Missing(what) => write!(f, "missing {what}"),
            Self::Invalid { what, value } => write!(f, "invalid {what}: {value:?}"),
            Self::UnknownReference(id) => write!(f, "unknown reference {id}"),
            Self::UnsupportedMask(mask) => write!(f, "mask version {mask:04X} is not supported"),
            Self::Unsupported(what) => write!(f, "not supported: {what}"),
            Self::TableOverflow(message) => write!(f, "table overflow: {message}"),
            Self::OutOfRange(message) => write!(f, "out of range: {message}"),
            Self::Archive(message) => f.write_str(message),
            Self::PasswordProtected => f.write_str(
                "the project is password-protected; export it from ETS without a password",
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Build an [`Error::Invalid`].
pub(crate) fn invalid(what: &'static str, value: &str) -> Error {
    Error::Invalid {
        what,
        value: value.chars().take(60).collect(),
    }
}
