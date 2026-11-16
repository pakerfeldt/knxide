//! The requests a download sends and the answers it reads, one level above bus frames.
//!
//! Both have a one-line text form, used to compare a plan with a recording:
//!
//! ```text
//! T_CONNECT
//! DEVICE_DESCRIPTOR_READ
//! AUTHORIZE_REQUEST FFFFFFFF
//! PROP_READ obj=0 pid=56 start=1 count=1
//! PROP_WRITE obj=1 pid=5 start=1 count=1 01000000000000000000
//! MEM_READ 4000 12
//! MEM_WRITE 0116 0F
//! RESTART
//! T_DISCONNECT
//! ```
//!
//! Answers mirror them: `DEVICE_DESCRIPTOR_RESPONSE 0701`, `AUTHORIZE_RESPONSE level=0`,
//! `PROP_RESPONSE obj=0 pid=56 start=1 count=0`, `MEM_RESPONSE 4000 0F`. Transport layer
//! acknowledgements are not modelled.

use std::fmt;
use std::str::FromStr;

/// One request to the device.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Request {
    /// `T_Connect`: open the transport connection. Spec: 3/3/4 §2.
    Connect,
    /// `T_Disconnect`: close the transport connection.
    Disconnect,
    /// `A_DeviceDescriptor_Read`, descriptor type 0: ask for the mask version. Spec: 3/3/7 §3.4.
    DeviceDescriptorRead,
    /// `A_Authorize_Request`: present an access key. Spec: 3/3/7 §3.5.
    Authorize {
        /// The key; `0xFFFFFFFF` is the default "no key".
        key: u32,
    },
    /// `A_PropertyValue_Read`. Spec: 3/3/7 §3.4.
    PropertyRead {
        /// Interface object index.
        object: u8,
        /// Property identifier.
        property: u8,
        /// First element, 1-based; 0 addresses the element count.
        start: u16,
        /// Number of elements.
        count: u8,
    },
    /// `A_PropertyValue_Write`.
    PropertyWrite {
        /// Interface object index.
        object: u8,
        /// Property identifier.
        property: u8,
        /// First element, 1-based.
        start: u16,
        /// Number of elements.
        count: u8,
        /// The element data.
        data: Vec<u8>,
    },
    /// `A_Memory_Read`. Spec: 3/3/7 §3.5.
    MemoryRead {
        /// Start address.
        address: u16,
        /// Number of bytes.
        count: u8,
    },
    /// `A_Memory_Write`.
    MemoryWrite {
        /// Start address.
        address: u16,
        /// The bytes.
        data: Vec<u8>,
    },
    /// `A_Restart`, basic restart.
    Restart,
}

/// One answer from the device.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Response {
    /// `A_DeviceDescriptor_Response`.
    DeviceDescriptor {
        /// Mask version.
        mask_version: u16,
    },
    /// `A_Authorize_Response`.
    Authorize {
        /// Granted access level; 0 is the most privileged.
        level: u8,
    },
    /// `A_PropertyValue_Response`. A count of 0 means the property is absent or protected.
    Property {
        /// Interface object index.
        object: u8,
        /// Property identifier.
        property: u8,
        /// First element.
        start: u16,
        /// Number of elements; 0 on error.
        count: u8,
        /// The element data.
        data: Vec<u8>,
    },
    /// `A_Memory_Response`.
    Memory {
        /// Start address.
        address: u16,
        /// The bytes.
        data: Vec<u8>,
    },
}

/// A request or response line could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLineError(String);

impl fmt::Display for ParseLineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot parse line {:?}", self.0)
    }
}

impl std::error::Error for ParseLineError {}

struct Hex<'d>(&'d [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02X}"))
    }
}

fn parse_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect => f.write_str("T_CONNECT"),
            Self::Disconnect => f.write_str("T_DISCONNECT"),
            Self::DeviceDescriptorRead => f.write_str("DEVICE_DESCRIPTOR_READ"),
            Self::Authorize { key } => write!(f, "AUTHORIZE_REQUEST {key:08X}"),
            Self::PropertyRead {
                object,
                property,
                start,
                count,
            } => write!(
                f,
                "PROP_READ obj={object} pid={property} start={start} count={count}"
            ),
            Self::PropertyWrite {
                object,
                property,
                start,
                count,
                data,
            } => write!(
                f,
                "PROP_WRITE obj={object} pid={property} start={start} count={count} {}",
                Hex(data)
            ),
            Self::MemoryRead { address, count } => write!(f, "MEM_READ {address:04X} {count}"),
            Self::MemoryWrite { address, data } => {
                write!(f, "MEM_WRITE {address:04X} {}", Hex(data))
            }
            Self::Restart => f.write_str("RESTART"),
        }
    }
}

impl fmt::Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceDescriptor { mask_version } => {
                write!(f, "DEVICE_DESCRIPTOR_RESPONSE {mask_version:04X}")
            }
            Self::Authorize { level } => write!(f, "AUTHORIZE_RESPONSE level={level}"),
            Self::Property {
                object,
                property,
                start,
                count,
                data,
            } => {
                write!(
                    f,
                    "PROP_RESPONSE obj={object} pid={property} start={start} count={count}"
                )?;
                if !data.is_empty() {
                    write!(f, " {}", Hex(data))?;
                }
                Ok(())
            }
            Self::Memory { address, data } => write!(f, "MEM_RESPONSE {address:04X} {}", Hex(data)),
        }
    }
}

/// The fields of a property line after its keyword.
struct PropertyFields {
    object: u8,
    property: u8,
    start: u16,
    count: u8,
    data: Vec<u8>,
}

fn property_fields<'w>(mut words: impl Iterator<Item = &'w str>) -> Option<PropertyFields> {
    let mut field = |name: &str| words.next()?.strip_prefix(name)?.strip_prefix('=');
    let object = field("obj")?.parse().ok()?;
    let property = field("pid")?.parse().ok()?;
    let start = field("start")?.parse().ok()?;
    let count = field("count")?.parse().ok()?;
    let data = match words.next() {
        Some(hex) => parse_hex(hex)?,
        None => Vec::new(),
    };
    words.next().is_none().then_some(PropertyFields {
        object,
        property,
        start,
        count,
        data,
    })
}

impl FromStr for Request {
    type Err = ParseLineError;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut words = line.split_whitespace();
        let parsed = (|| {
            let request = match words.next()? {
                "T_CONNECT" => Self::Connect,
                "T_DISCONNECT" => Self::Disconnect,
                "DEVICE_DESCRIPTOR_READ" => Self::DeviceDescriptorRead,
                "RESTART" => Self::Restart,
                "AUTHORIZE_REQUEST" => Self::Authorize {
                    key: u32::from_str_radix(words.next()?, 16).ok()?,
                },
                "MEM_READ" => Self::MemoryRead {
                    address: u16::from_str_radix(words.next()?, 16).ok()?,
                    count: words.next()?.parse().ok()?,
                },
                "MEM_WRITE" => Self::MemoryWrite {
                    address: u16::from_str_radix(words.next()?, 16).ok()?,
                    data: parse_hex(words.next()?)?,
                },
                "PROP_READ" => {
                    let fields = property_fields(&mut words)?;
                    if !fields.data.is_empty() {
                        return None;
                    }
                    Self::PropertyRead {
                        object: fields.object,
                        property: fields.property,
                        start: fields.start,
                        count: fields.count,
                    }
                }
                "PROP_WRITE" => {
                    let fields = property_fields(&mut words)?;
                    Self::PropertyWrite {
                        object: fields.object,
                        property: fields.property,
                        start: fields.start,
                        count: fields.count,
                        data: fields.data,
                    }
                }
                _ => return None,
            };
            words.next().is_none().then_some(request)
        })();
        parsed.ok_or_else(|| ParseLineError(line.to_string()))
    }
}

impl FromStr for Response {
    type Err = ParseLineError;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut words = line.split_whitespace();
        let parsed = (|| {
            let response = match words.next()? {
                "DEVICE_DESCRIPTOR_RESPONSE" => Self::DeviceDescriptor {
                    mask_version: u16::from_str_radix(words.next()?, 16).ok()?,
                },
                "AUTHORIZE_RESPONSE" => Self::Authorize {
                    level: words.next()?.strip_prefix("level=")?.parse().ok()?,
                },
                "MEM_RESPONSE" => Self::Memory {
                    address: u16::from_str_radix(words.next()?, 16).ok()?,
                    data: parse_hex(words.next()?)?,
                },
                "PROP_RESPONSE" => {
                    let fields = property_fields(&mut words)?;
                    Self::Property {
                        object: fields.object,
                        property: fields.property,
                        start: fields.start,
                        count: fields.count,
                        data: fields.data,
                    }
                }
                _ => return None,
            };
            words.next().is_none().then_some(response)
        })();
        parsed.ok_or_else(|| ParseLineError(line.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lines_round_trip() {
        for line in [
            "T_CONNECT",
            "T_DISCONNECT",
            "DEVICE_DESCRIPTOR_READ",
            "AUTHORIZE_REQUEST FFFFFFFF",
            "PROP_READ obj=0 pid=56 start=1 count=1",
            "PROP_WRITE obj=1 pid=5 start=1 count=1 03000116002031038000",
            "MEM_READ B6EA 1",
            "MEM_WRITE 0104 1400000000000000000000",
            "RESTART",
        ] {
            let request: Request = line.parse().unwrap();
            assert_eq!(request.to_string(), line);
        }
    }

    #[test]
    fn response_lines_round_trip() {
        for line in [
            "DEVICE_DESCRIPTOR_RESPONSE 0021",
            "AUTHORIZE_RESPONSE level=0",
            "PROP_RESPONSE obj=0 pid=56 start=1 count=0",
            "PROP_RESPONSE obj=0 pid=14 start=1 count=1 04",
            "MEM_RESPONSE 4003 090A090B",
        ] {
            let response: Response = line.parse().unwrap();
            assert_eq!(response.to_string(), line);
        }
    }

    #[test]
    fn bad_lines() {
        for line in [
            "",
            "HELLO",
            "T_CONNECT now",
            "MEM_READ 4000",
            "MEM_READ 4000 300",
            "MEM_WRITE 4000 0",
            "MEM_WRITE 4000 0G",
            "PROP_READ obj=0 pid=56 start=1",
            "PROP_READ obj=0 pid=56 start=1 count=1 00",
            "PROP_READ pid=56 obj=0 start=1 count=1",
            "AUTHORIZE_REQUEST",
        ] {
            assert!(line.parse::<Request>().is_err(), "{line:?}");
        }
        assert!("MEM_RESPONSE 4000".parse::<Response>().is_err());
        assert!("AUTHORIZE_RESPONSE 0".parse::<Response>().is_err());
    }
}
