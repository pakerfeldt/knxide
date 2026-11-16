//! Requests and responses as application layer PDUs, both ways.
//!
//! [`request_apdu`] and [`response_from_apdu`] are the tool's side; [`request_from_apdu`] and
//! [`response_apdu`] are the device's side and exist for simulated devices. The transport
//! requests (`T_Connect`, `T_Disconnect`) have no APDU: they are frames of the transport layer
//! ([`crate::transport`]).
//!
//! Layouts follow the KNX standard, 3/3/7 (Application Layer): §3.4 for the device descriptor
//! and property services, §3.5 for memory and authorisation, §3.7 for the restart. The request
//! layouts are verified against the recorded downloads this crate reproduces.

use std::fmt;

use knxide_core::Apdu;
use knxide_core::apdu::{
    APCI_AUTHORIZE_REQUEST, APCI_AUTHORIZE_RESPONSE, APCI_DEVICE_DESCRIPTOR_READ,
    APCI_DEVICE_DESCRIPTOR_RESPONSE, APCI_MEMORY_READ, APCI_MEMORY_RESPONSE, APCI_MEMORY_WRITE,
    APCI_PROPERTY_VALUE_READ, APCI_PROPERTY_VALUE_RESPONSE, APCI_PROPERTY_VALUE_WRITE,
    APCI_RESTART,
};

use crate::request::{Request, Response};

/// Most bytes one memory service addresses: the count travels in six bits.
pub const MAX_MEMORY_COUNT: usize = 63;
/// Most elements one property service addresses: the count travels in four bits.
pub const MAX_PROPERTY_COUNT: u8 = 15;
/// Highest property start index: it travels in twelve bits.
pub const MAX_PROPERTY_START: u16 = 0x0FFF;

/// A request or response cannot be put into an APDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// `T_Connect` and `T_Disconnect` are transport layer frames, not APDUs.
    TransportRequest,
    /// A field does not fit its place in the APDU. The string names it.
    OutOfRange(&'static str),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TransportRequest => f.write_str("a transport request has no APDU"),
            Self::OutOfRange(what) => write!(f, "{what} does not fit the APDU"),
        }
    }
}

impl std::error::Error for WireError {}

fn other(apci: u16, data: Vec<u8>) -> Apdu {
    Apdu::Other { apci, data }
}

/// Object index, property identifier, then four bits of element count and twelve bits of
/// start index.
fn property_header(object: u8, property: u8, start: u16, count: u8) -> Result<Vec<u8>, WireError> {
    if count > MAX_PROPERTY_COUNT {
        return Err(WireError::OutOfRange("property element count"));
    }
    if start > MAX_PROPERTY_START {
        return Err(WireError::OutOfRange("property start index"));
    }
    let [high, low] = start.to_be_bytes();
    Ok(vec![object, property, count << 4 | high, low])
}

/// The memory APCI with the byte count in its low six bits.
fn memory_apci(base: u16, count: usize) -> Result<u16, WireError> {
    u16::try_from(count)
        .ok()
        .filter(|count| usize::from(*count) <= MAX_MEMORY_COUNT)
        .map(|count| base | count)
        .ok_or(WireError::OutOfRange("memory byte count"))
}

/// The APDU of a request.
pub fn request_apdu(request: &Request) -> Result<Apdu, WireError> {
    Ok(match request {
        Request::Connect | Request::Disconnect => return Err(WireError::TransportRequest),
        // Descriptor type 0: the mask version.
        Request::DeviceDescriptorRead => other(APCI_DEVICE_DESCRIPTOR_READ, Vec::new()),
        Request::Authorize { key } => {
            let mut data = vec![0];
            data.extend(key.to_be_bytes());
            other(APCI_AUTHORIZE_REQUEST, data)
        }
        Request::PropertyRead {
            object,
            property,
            start,
            count,
        } => other(
            APCI_PROPERTY_VALUE_READ,
            property_header(*object, *property, *start, *count)?,
        ),
        Request::PropertyWrite {
            object,
            property,
            start,
            count,
            data,
        } => {
            let mut bytes = property_header(*object, *property, *start, *count)?;
            bytes.extend(data);
            other(APCI_PROPERTY_VALUE_WRITE, bytes)
        }
        Request::MemoryRead { address, count } => other(
            memory_apci(APCI_MEMORY_READ, usize::from(*count))?,
            address.to_be_bytes().to_vec(),
        ),
        Request::MemoryWrite { address, data } => {
            let mut bytes = address.to_be_bytes().to_vec();
            bytes.extend(data);
            other(memory_apci(APCI_MEMORY_WRITE, data.len())?, bytes)
        }
        Request::Restart => other(APCI_RESTART, Vec::new()),
    })
}

/// The APDU of a response.
pub fn response_apdu(response: &Response) -> Result<Apdu, WireError> {
    Ok(match response {
        Response::DeviceDescriptor { mask_version } => other(
            APCI_DEVICE_DESCRIPTOR_RESPONSE,
            mask_version.to_be_bytes().to_vec(),
        ),
        Response::Authorize { level } => other(APCI_AUTHORIZE_RESPONSE, vec![*level]),
        Response::Property {
            object,
            property,
            start,
            count,
            data,
        } => {
            let mut bytes = property_header(*object, *property, *start, *count)?;
            bytes.extend(data);
            other(APCI_PROPERTY_VALUE_RESPONSE, bytes)
        }
        Response::Memory { address, data } => {
            let mut bytes = address.to_be_bytes().to_vec();
            bytes.extend(data);
            other(memory_apci(APCI_MEMORY_RESPONSE, data.len())?, bytes)
        }
    })
}

/// The raw APCI and data of an APDU that is not a group value service.
fn raw(apdu: &Apdu) -> Option<(u16, &[u8])> {
    match apdu {
        Apdu::Other { apci, data } => Some((*apci, data)),
        _ => None,
    }
}

/// Split a property service body into its header fields and element data.
fn property_fields(data: &[u8]) -> Option<(u8, u8, u16, u8, &[u8])> {
    let [object, property, mixed, low, rest @ ..] = data else {
        return None;
    };
    let start = u16::from_be_bytes([mixed & 0x0F, *low]);
    Some((*object, *property, start, mixed >> 4, rest))
}

/// Split a memory service into address and data, given the APCI base it must have. The byte
/// count in the APCI must equal the number of bytes that follow the address, except for a read,
/// which carries none.
fn memory_fields(apci: u16, base: u16, data: &[u8]) -> Option<(u16, usize, &[u8])> {
    if apci & !0x3F != base {
        return None;
    }
    let [high, low, rest @ ..] = data else {
        return None;
    };
    Some((
        u16::from_be_bytes([*high, *low]),
        usize::from(apci & 0x3F),
        rest,
    ))
}

/// The response an APDU carries, or `None` when it is not one of the four answers a download
/// reads (or is malformed).
pub fn response_from_apdu(apdu: &Apdu) -> Option<Response> {
    let (apci, data) = raw(apdu)?;
    match apci {
        APCI_DEVICE_DESCRIPTOR_RESPONSE => match data {
            [high, low] => Some(Response::DeviceDescriptor {
                mask_version: u16::from_be_bytes([*high, *low]),
            }),
            _ => None,
        },
        APCI_AUTHORIZE_RESPONSE => match data {
            [level] => Some(Response::Authorize { level: *level }),
            _ => None,
        },
        APCI_PROPERTY_VALUE_RESPONSE => {
            let (object, property, start, count, rest) = property_fields(data)?;
            Some(Response::Property {
                object,
                property,
                start,
                count,
                data: rest.to_vec(),
            })
        }
        _ => {
            let (address, count, rest) = memory_fields(apci, APCI_MEMORY_RESPONSE, data)?;
            (count == rest.len()).then(|| Response::Memory {
                address,
                data: rest.to_vec(),
            })
        }
    }
}

/// The request an APDU carries, or `None` when it is not one a download sends (or is
/// malformed).
pub fn request_from_apdu(apdu: &Apdu) -> Option<Request> {
    let (apci, data) = raw(apdu)?;
    match apci {
        APCI_DEVICE_DESCRIPTOR_READ => data.is_empty().then_some(Request::DeviceDescriptorRead),
        APCI_RESTART => data.is_empty().then_some(Request::Restart),
        APCI_AUTHORIZE_REQUEST => match data {
            [_, k1, k2, k3, k4] => Some(Request::Authorize {
                key: u32::from_be_bytes([*k1, *k2, *k3, *k4]),
            }),
            _ => None,
        },
        APCI_PROPERTY_VALUE_READ => {
            let (object, property, start, count, rest) = property_fields(data)?;
            rest.is_empty().then_some(Request::PropertyRead {
                object,
                property,
                start,
                count,
            })
        }
        APCI_PROPERTY_VALUE_WRITE => {
            let (object, property, start, count, rest) = property_fields(data)?;
            Some(Request::PropertyWrite {
                object,
                property,
                start,
                count,
                data: rest.to_vec(),
            })
        }
        _ => {
            if let Some((address, count, rest)) = memory_fields(apci, APCI_MEMORY_READ, data) {
                return rest.is_empty().then_some(Request::MemoryRead {
                    address,
                    count: count as u8,
                });
            }
            let (address, count, rest) = memory_fields(apci, APCI_MEMORY_WRITE, data)?;
            (count == rest.len()).then(|| Request::MemoryWrite {
                address,
                data: rest.to_vec(),
            })
        }
    }
}

/// Whether `response` can be the answer to `request`: the same service family, and the same
/// object and property or the same address. A memory write is answered by an echo of its
/// address and bytes.
pub fn answers(request: &Request, response: &Response) -> bool {
    match (request, response) {
        (Request::DeviceDescriptorRead, Response::DeviceDescriptor { .. })
        | (Request::Authorize { .. }, Response::Authorize { .. }) => true,
        (
            Request::PropertyRead {
                object, property, ..
            }
            | Request::PropertyWrite {
                object, property, ..
            },
            Response::Property {
                object: o,
                property: p,
                ..
            },
        ) => object == o && property == p,
        (
            Request::MemoryRead { address, .. },
            Response::Memory {
                address: answered, ..
            },
        ) => address == answered,
        (
            Request::MemoryWrite { address, data },
            Response::Memory {
                address: answered,
                data: echoed,
            },
        ) => address == answered && data == echoed,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use knxide_core::Tpdu;

    use super::*;

    /// The octets of a numbered data frame with sequence number 0 that carries `apdu`.
    fn octets(apdu: Apdu) -> Vec<u8> {
        let mut out = Vec::new();
        Tpdu::Connected { sequence: 0, apdu }
            .encode(&mut out)
            .unwrap();
        out
    }

    fn request(line: &str) -> Vec<u8> {
        octets(request_apdu(&line.parse().unwrap()).unwrap())
    }

    fn response(line: &str) -> Vec<u8> {
        octets(response_apdu(&line.parse().unwrap()).unwrap())
    }

    #[test]
    fn request_octets() {
        assert_eq!(request("DEVICE_DESCRIPTOR_READ"), [0x43, 0x00]);
        assert_eq!(
            request("AUTHORIZE_REQUEST FFFFFFFF"),
            [0x43, 0xD1, 0x00, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(
            request("PROP_READ obj=0 pid=56 start=1 count=1"),
            [0x43, 0xD5, 0x00, 0x38, 0x10, 0x01]
        );
        assert_eq!(
            request("PROP_WRITE obj=1 pid=5 start=1 count=1 01000000000000000000"),
            [
                0x43, 0xD7, 0x01, 0x05, 0x10, 0x01, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(request("MEM_READ 4000 12"), [0x42, 0x0C, 0x40, 0x00]);
        assert_eq!(
            request("MEM_WRITE 0116 0F01"),
            [0x42, 0x82, 0x01, 0x16, 0x0F, 0x01]
        );
        assert_eq!(request("RESTART"), [0x43, 0x80]);
    }

    #[test]
    fn response_octets() {
        assert_eq!(
            response("DEVICE_DESCRIPTOR_RESPONSE 0705"),
            [0x43, 0x40, 0x07, 0x05]
        );
        assert_eq!(response("AUTHORIZE_RESPONSE level=2"), [0x43, 0xD2, 0x02]);
        assert_eq!(
            response("PROP_RESPONSE obj=0 pid=56 start=1 count=0"),
            [0x43, 0xD6, 0x00, 0x38, 0x00, 0x01]
        );
        assert_eq!(
            response("MEM_RESPONSE 4003 090A"),
            [0x42, 0x42, 0x40, 0x03, 0x09, 0x0A]
        );
    }

    #[test]
    fn a_full_frame_stays_within_apdu_length_15() {
        // Twelve bytes of memory data make an APDU of 15 octets after the TPCI octet.
        let write = request("MEM_WRITE 4000 000102030405060708090A0B");
        assert_eq!(write.len() - 1, 15);
        let event = request("PROP_WRITE obj=1 pid=5 start=1 count=1 03000116002031038000");
        assert_eq!(event.len() - 1, 15);
    }

    #[test]
    fn transport_requests_and_oversized_fields_are_refused() {
        assert_eq!(
            request_apdu(&Request::Connect),
            Err(WireError::TransportRequest)
        );
        assert_eq!(
            request_apdu(&Request::Disconnect),
            Err(WireError::TransportRequest)
        );
        assert!(
            request_apdu(&Request::MemoryRead {
                address: 0,
                count: 64
            })
            .is_err()
        );
        assert!(
            request_apdu(&Request::MemoryWrite {
                address: 0,
                data: vec![0; 64]
            })
            .is_err()
        );
        assert!(
            request_apdu(&Request::PropertyRead {
                object: 0,
                property: 1,
                start: 1,
                count: 16
            })
            .is_err()
        );
        assert!(
            request_apdu(&Request::PropertyRead {
                object: 0,
                property: 1,
                start: 0x1000,
                count: 1
            })
            .is_err()
        );
    }

    #[test]
    fn malformed_apdus_are_not_responses() {
        for (apci, data) in [
            (APCI_DEVICE_DESCRIPTOR_RESPONSE, vec![0x07]),
            (APCI_DEVICE_DESCRIPTOR_RESPONSE | 2, vec![0x07, 0x05]),
            (APCI_AUTHORIZE_RESPONSE, vec![]),
            (APCI_PROPERTY_VALUE_RESPONSE, vec![0, 56, 0x10]),
            (APCI_MEMORY_RESPONSE | 2, vec![0x40, 0x00, 0x01]),
            (APCI_MEMORY_RESPONSE, vec![0x40]),
            (APCI_MEMORY_WRITE | 1, vec![0x40, 0x00, 0x01]),
            (0x3FF, vec![]),
        ] {
            assert_eq!(response_from_apdu(&other(apci, data.clone())), None);
        }
        assert_eq!(response_from_apdu(&Apdu::GroupValueRead), None);
        assert_eq!(request_from_apdu(&Apdu::GroupValueRead), None);
        assert_eq!(
            request_from_apdu(&other(APCI_MEMORY_WRITE | 3, vec![0, 0, 1])),
            None
        );
    }

    #[test]
    fn which_response_answers_which_request() {
        let read: Request = "MEM_READ 4000 2".parse().unwrap();
        assert!(answers(&read, &"MEM_RESPONSE 4000 0102".parse().unwrap()));
        assert!(!answers(&read, &"MEM_RESPONSE 4002 0102".parse().unwrap()));
        let write: Request = "MEM_WRITE 4000 0102".parse().unwrap();
        assert!(answers(&write, &"MEM_RESPONSE 4000 0102".parse().unwrap()));
        assert!(!answers(&write, &"MEM_RESPONSE 4000 0103".parse().unwrap()));
        let property: Request = "PROP_READ obj=3 pid=13 start=1 count=1".parse().unwrap();
        assert!(answers(
            &property,
            &"PROP_RESPONSE obj=3 pid=13 start=1 count=0"
                .parse()
                .unwrap()
        ));
        assert!(!answers(
            &property,
            &"PROP_RESPONSE obj=0 pid=13 start=1 count=0"
                .parse()
                .unwrap()
        ));
        assert!(!answers(
            &Request::Restart,
            &"AUTHORIZE_RESPONSE level=0".parse().unwrap()
        ));
    }
}
