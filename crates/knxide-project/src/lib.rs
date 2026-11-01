//! Import group addresses and their datapoint types from ETS.
//!
//! The bus carries no type information, so an application needs to know which DPT each
//! group address uses. ETS has it; this crate reads ETS's group address exports:
//!
//! - CSV ("Export Group Addresses", either the 1/1 or the 3/1 layout, any separator).
//! - XML (the same export in XML format).
//!
//! Parsing works on text; reading the file is up to the caller ([`GroupAddressTable::parse_bytes`]
//! handles the encodings ETS writes).
//!
//! ```
//! use knxide_core::ga;
//! use knxide_project::GroupAddressTable;
//!
//! let csv = "\"Group name\";\"Address\";\"Description\";\"DatapointType\"\n\
//!            \"Living temperature\";\"1/2/3\";\"\";\"DPST-9-1\"\n";
//! let table = GroupAddressTable::parse(csv).unwrap();
//! let entry = table.get(ga!(1/2/3)).unwrap();
//! assert_eq!(entry.name, "Living temperature");
//! assert_eq!(entry.dpt.unwrap().to_string(), "9.001");
//! ```

mod csv;
mod xml;

use std::collections::BTreeMap;
use std::fmt;

use knxide_core::GroupAddress;
use knxide_core::dpt::DptId;

/// One group address from the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupAddressInfo {
    /// The address.
    pub address: GroupAddress,
    /// Its name in ETS.
    pub name: String,
    /// Its description, possibly empty.
    pub description: String,
    /// Datapoint type, if one is assigned in ETS. Exports may list several; this is the first.
    pub dpt: Option<DptId>,
    /// Names of the enclosing main and middle groups (ranges), outermost first.
    pub path: Vec<String>,
}

/// Group addresses indexed by address.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupAddressTable {
    entries: BTreeMap<GroupAddress, GroupAddressInfo>,
}

/// An export could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The input is empty.
    Empty,
    /// The input is not a recognised ETS group address export.
    UnknownFormat,
    /// The XML is malformed.
    Xml(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the export is empty"),
            Self::UnknownFormat => f.write_str("not an ETS group address export (CSV or XML)"),
            Self::Xml(msg) => write!(f, "malformed XML: {msg}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl GroupAddressTable {
    /// Parse a CSV or XML export, detecting which.
    pub fn parse(text: &str) -> Result<Self, ImportError> {
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let trimmed = text.trim_start();
        if trimmed.is_empty() {
            return Err(ImportError::Empty);
        }
        if trimmed.starts_with('<') {
            Self::from_xml(text)
        } else {
            Self::from_csv(text)
        }
    }

    /// Parse raw file contents: UTF-8 (with or without BOM), UTF-16 with BOM, or else
    /// ISO 8859-1 (ETS's "ANSI" option).
    pub fn parse_bytes(bytes: &[u8]) -> Result<Self, ImportError> {
        Self::parse(&decode_text(bytes))
    }

    /// Parse a CSV export.
    pub fn from_csv(text: &str) -> Result<Self, ImportError> {
        csv::parse(text.strip_prefix('\u{FEFF}').unwrap_or(text))
    }

    /// Parse an XML export.
    pub fn from_xml(text: &str) -> Result<Self, ImportError> {
        xml::parse(text.strip_prefix('\u{FEFF}').unwrap_or(text))
    }

    /// Add or replace an entry.
    pub fn insert(&mut self, info: GroupAddressInfo) {
        self.entries.insert(info.address, info);
    }

    /// The entry for `address`.
    pub fn get(&self, address: GroupAddress) -> Option<&GroupAddressInfo> {
        self.entries.get(&address)
    }

    /// The DPT of `address`, if known.
    pub fn dpt(&self, address: GroupAddress) -> Option<DptId> {
        self.get(address).and_then(|i| i.dpt)
    }

    /// Entries in address order.
    pub fn iter(&self) -> impl Iterator<Item = &GroupAddressInfo> {
        self.entries.values()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Parse ETS's DPT notation: `DPST-9-1`, `DPT-9`, `9.001`. Lists (`DPST-1-1,DPST-1-2`) yield
/// the first entry. Empty or unknown text yields `None`.
fn parse_dpt(text: &str) -> Option<DptId> {
    let first = text
        .split([',', ' '])
        .map(str::trim)
        .find(|s| !s.is_empty())?;
    first.parse().ok()
}

fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let utf16 = |rest: &[u8], le: bool| {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| {
                if le {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}
