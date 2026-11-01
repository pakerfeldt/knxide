//! Transport and application layer PDUs: TPCI, APCI and group value payloads.
//!
//! A TPDU starts with the TPCI octet, whose low two bits carry the top of the 10-bit APCI. The
//! next octet carries the rest of the APCI, and its low 6 bits double as data for small values.
//! Spec: 3/3/4 (Transport Layer) §2 for the TPCI, 3/3/7 (Application Layer) §3.1 for the APCI.

use alloc::vec::Vec;

use crate::error::{DecodeError, EncodeError};
use crate::reader::Reader;

/// Largest value that fits in the 6 data bits of the APCI octet.
pub const SMALL_PAYLOAD_MAX: u8 = 0x3F;

/// Value carried by `GroupValue_Write` and `GroupValue_Response`.
///
/// Datapoint types of 6 bits or less (DPT 1, 2, 3) use [`Payload::Small`] and travel inside
/// the APCI octet. Everything else, including one-byte types such as DPT 5, uses
/// [`Payload::Bytes`] and is appended after it, even when the value would fit in 6 bits.
/// Spec: 3/3/7 §3.1.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Payload {
    /// Up to 6 bits ([`SMALL_PAYLOAD_MAX`]) stored in the APCI octet.
    Small(u8),
    /// Octets appended after the APCI.
    Bytes(Vec<u8>),
}

impl Payload {
    /// Parse a raw value as a person would type it: `0x0c1a` (bytes, hex) or `0`–`63` (small).
    pub fn parse_raw(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            if hex.is_empty() || hex.len() % 2 != 0 {
                return None;
            }
            let bytes: Option<Vec<u8>> = (0..hex.len())
                .step_by(2)
                .map(|i| {
                    hex.get(i..i + 2)
                        .and_then(|b| u8::from_str_radix(b, 16).ok())
                })
                .collect();
            return bytes.map(Payload::Bytes);
        }
        match text.parse::<u8>() {
            Ok(v) if v <= SMALL_PAYLOAD_MAX => Some(Payload::Small(v)),
            _ => None,
        }
    }
}

impl core::fmt::Display for Payload {
    /// `0x01` for small values, `0c 1a` for bytes.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Payload::Small(v) => write!(f, "{v:#04x}"),
            Payload::Bytes(bytes) => {
                for (i, b) in bytes.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
        }
    }
}

/// Application layer PDU.
///
/// Only the group value services are decoded. Anything else is kept as [`Apdu::Other`] so it
/// can be passed through unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Apdu {
    /// `A_GroupValue_Read`, APCI `0x000`.
    GroupValueRead,
    /// `A_GroupValue_Response`, APCI `0x040`.
    GroupValueResponse(Payload),
    /// `A_GroupValue_Write`, APCI `0x080`.
    GroupValueWrite(Payload),
    /// Any other service, raw.
    Other {
        /// Full 10-bit APCI, including any data in its low 6 bits.
        apci: u16,
        /// Octets after the APCI.
        data: Vec<u8>,
    },
}

/// `A_Restart`, basic restart: APCI `0x380` with no data. The device restarts its application
/// and keeps its configuration. Spec: 3/3/7 §3.7.
pub const APCI_RESTART: u16 = 0x380;
/// `A_Restart`, master reset: APCI `0x381` plus an erase code. Can wipe the device's
/// configuration; knxide never sends it.
pub const APCI_RESTART_MASTER_RESET: u16 = 0x381;

/// 4-bit APCI codes of the group value services (the top 4 bits of the 10-bit APCI).
const APCI_GROUP_VALUE_READ: u16 = 0b0000;
const APCI_GROUP_VALUE_RESPONSE: u16 = 0b0001;
const APCI_GROUP_VALUE_WRITE: u16 = 0b0010;

impl Apdu {
    /// `A_Restart` (basic restart). Send it to a device over a transport connection.
    pub fn restart() -> Self {
        Self::Other {
            apci: APCI_RESTART,
            data: Vec::new(),
        }
    }

    fn decode(apci: u16, data: &[u8]) -> Self {
        let small = (apci & 0x3F) as u8;
        let payload = || {
            if data.is_empty() {
                Some(Payload::Small(small))
            } else if small == 0 {
                Some(Payload::Bytes(data.to_vec()))
            } else {
                // Long payload with stray bits in the APCI octet: not a well-formed group
                // value, keep it raw so re-encoding reproduces it.
                None
            }
        };
        let decoded = match apci >> 6 {
            APCI_GROUP_VALUE_READ if small == 0 && data.is_empty() => Some(Self::GroupValueRead),
            APCI_GROUP_VALUE_RESPONSE => payload().map(Self::GroupValueResponse),
            APCI_GROUP_VALUE_WRITE => payload().map(Self::GroupValueWrite),
            _ => None,
        };
        decoded.unwrap_or_else(|| Self::Other {
            apci,
            data: data.to_vec(),
        })
    }

    /// The 10-bit APCI and the octets that follow it.
    fn parts(&self) -> Result<(u16, &[u8]), EncodeError> {
        match self {
            Self::GroupValueRead => Ok((APCI_GROUP_VALUE_READ << 6, &[])),
            Self::GroupValueResponse(p) => payload_parts(APCI_GROUP_VALUE_RESPONSE, p),
            Self::GroupValueWrite(p) => payload_parts(APCI_GROUP_VALUE_WRITE, p),
            Self::Other { apci, .. } if *apci > 0x03FF => Err(EncodeError::OutOfRange("APCI")),
            Self::Other { apci, data } => Ok((*apci, data.as_slice())),
        }
    }
}

fn payload_parts(code: u16, payload: &Payload) -> Result<(u16, &[u8]), EncodeError> {
    match payload {
        Payload::Small(v) if *v > SMALL_PAYLOAD_MAX => {
            Err(EncodeError::OutOfRange("small payload"))
        }
        Payload::Small(v) => Ok((code << 6 | u16::from(*v), &[])),
        Payload::Bytes(bytes) => Ok((code << 6, bytes.as_slice())),
    }
}

/// Transport layer PDU: the TPCI plus, for data PDUs, the APDU.
///
/// Which `T_Data_*` service an unnumbered [`Tpdu::Data`] is (group, broadcast or individual)
/// follows from the frame's destination. Spec: 3/3/4 §2.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Tpdu {
    /// Unnumbered data: `T_Data_Group`, `T_Data_Broadcast` or `T_Data_Individual`. TPCI `000000`.
    Data(Apdu),
    /// `T_Data_Tag_Group`, used by KNX Data Secure. TPCI `000001`.
    TagGroup(Apdu),
    /// `T_Data_Connected`, numbered data on a transport connection. TPCI `01ssss`.
    Connected {
        /// 4-bit sequence number.
        sequence: u8,
        /// The application PDU.
        apdu: Apdu,
    },
    /// `T_Connect`, octet `0x80`.
    Connect,
    /// `T_Disconnect`, octet `0x81`.
    Disconnect,
    /// `T_ACK`, octet `11ssss10`.
    Ack {
        /// 4-bit sequence number.
        sequence: u8,
    },
    /// `T_NAK`, octet `11ssss11`.
    Nak {
        /// 4-bit sequence number.
        sequence: u8,
    },
}

impl Tpdu {
    /// Decode a complete TPDU: the TPCI octet plus exactly the octets the NPDU length covers.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let tpci = r.u8()?;
        let sequence = (tpci >> 2) & 0x0F;
        let numbered = tpci & 0x40 != 0;
        if tpci & 0x80 != 0 {
            if r.remaining() != 0 {
                return Err(DecodeError::Invalid("control TPDU length"));
            }
            return match (numbered, tpci & 0x03) {
                (false, 0b00) if sequence == 0 => Ok(Self::Connect),
                (false, 0b01) if sequence == 0 => Ok(Self::Disconnect),
                (true, 0b10) => Ok(Self::Ack { sequence }),
                (true, 0b11) => Ok(Self::Nak { sequence }),
                _ => Err(DecodeError::Invalid("TPCI")),
            };
        }
        let apci = u16::from(tpci & 0x03) << 8 | u16::from(r.u8()?);
        let apdu = Apdu::decode(apci, r.rest());
        if numbered {
            return Ok(Self::Connected { sequence, apdu });
        }
        match tpci & 0xFC {
            0x00 => Ok(Self::Data(apdu)),
            0x04 => Ok(Self::TagGroup(apdu)),
            _ => Err(DecodeError::Invalid("TPCI")),
        }
    }

    /// Append the TPDU octets to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let check_seq = |sequence: u8| {
            if sequence > 0x0F {
                Err(EncodeError::OutOfRange("sequence number"))
            } else {
                Ok(sequence << 2)
            }
        };
        let (tpci, apdu) = match self {
            Self::Data(apdu) => (0x00, apdu),
            Self::TagGroup(apdu) => (0x04, apdu),
            Self::Connected { sequence, apdu } => (0x40 | check_seq(*sequence)?, apdu),
            Self::Connect => {
                out.push(0x80);
                return Ok(());
            }
            Self::Disconnect => {
                out.push(0x81);
                return Ok(());
            }
            Self::Ack { sequence } => {
                out.push(0xC2 | check_seq(*sequence)?);
                return Ok(());
            }
            Self::Nak { sequence } => {
                out.push(0xC3 | check_seq(*sequence)?);
                return Ok(());
            }
        };
        let (apci, data) = apdu.parts()?;
        out.push(tpci | (apci >> 8) as u8);
        out.push(apci as u8);
        out.extend_from_slice(data);
        Ok(())
    }

    /// The APDU, if this is a data PDU.
    pub fn apdu(&self) -> Option<&Apdu> {
        match self {
            Self::Data(apdu) | Self::TagGroup(apdu) | Self::Connected { apdu, .. } => Some(apdu),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn enc(tpdu: &Tpdu) -> Vec<u8> {
        let mut out = Vec::new();
        tpdu.encode(&mut out).unwrap();
        out
    }

    #[test]
    fn group_value_examples() {
        // Brief §3.8 / spec 3/3/7 §3.1.
        assert_eq!(enc(&Tpdu::Data(Apdu::GroupValueRead)), [0x00, 0x00]);
        assert_eq!(
            enc(&Tpdu::Data(Apdu::GroupValueWrite(Payload::Small(1)))),
            [0x00, 0x81]
        );
        assert_eq!(
            enc(&Tpdu::Data(Apdu::GroupValueResponse(Payload::Small(0)))),
            [0x00, 0x40]
        );
        assert_eq!(
            enc(&Tpdu::Data(Apdu::GroupValueWrite(Payload::Bytes(vec![
                0x0C, 0x1A
            ])))),
            [0x00, 0x80, 0x0C, 0x1A]
        );
        // A one-byte DPT 5 value is appended, never short-encoded, even when small.
        assert_eq!(
            enc(&Tpdu::Data(Apdu::GroupValueWrite(Payload::Bytes(vec![
                0x01
            ])))),
            [0x00, 0x80, 0x01]
        );
    }

    #[test]
    fn decode_group_values() {
        assert_eq!(
            Tpdu::decode(&[0x00, 0x00]),
            Ok(Tpdu::Data(Apdu::GroupValueRead))
        );
        assert_eq!(
            Tpdu::decode(&[0x00, 0x81]),
            Ok(Tpdu::Data(Apdu::GroupValueWrite(Payload::Small(1))))
        );
        assert_eq!(
            Tpdu::decode(&[0x00, 0x40, 0x7F]),
            Ok(Tpdu::Data(Apdu::GroupValueResponse(Payload::Bytes(vec![
                0x7F
            ]))))
        );
    }

    #[test]
    fn control_pdus() {
        assert_eq!(enc(&Tpdu::Connect), [0x80]);
        assert_eq!(enc(&Tpdu::Disconnect), [0x81]);
        assert_eq!(enc(&Tpdu::Ack { sequence: 3 }), [0xCE]);
        assert_eq!(enc(&Tpdu::Nak { sequence: 15 }), [0xFF]);
        assert_eq!(Tpdu::decode(&[0xCE]), Ok(Tpdu::Ack { sequence: 3 }));
        assert_eq!(Tpdu::decode(&[0x84]), Err(DecodeError::Invalid("TPCI")));
        assert_eq!(
            Tpdu::decode(&[0x80, 0x00]),
            Err(DecodeError::Invalid("control TPDU length"))
        );
    }

    #[test]
    fn rejects_bad_values() {
        let mut out = Vec::new();
        let small = Tpdu::Data(Apdu::GroupValueWrite(Payload::Small(0x40)));
        assert_eq!(
            small.encode(&mut out),
            Err(EncodeError::OutOfRange("small payload"))
        );
        assert_eq!(Tpdu::decode(&[]), Err(DecodeError::UnexpectedEnd));
        assert_eq!(Tpdu::decode(&[0x00]), Err(DecodeError::UnexpectedEnd));
        assert_eq!(
            Tpdu::decode(&[0x08, 0x00]),
            Err(DecodeError::Invalid("TPCI"))
        );
    }

    #[test]
    fn raw_payloads() {
        use alloc::string::ToString;
        assert_eq!(Payload::parse_raw("1"), Some(Payload::Small(1)));
        assert_eq!(
            Payload::parse_raw("0x0c1A"),
            Some(Payload::Bytes(vec![0x0c, 0x1a]))
        );
        for bad in ["64", "0x", "0x0c1", "0xzz", "-1", "on"] {
            assert_eq!(Payload::parse_raw(bad), None, "{bad}");
        }
        assert_eq!(Payload::Small(1).to_string(), "0x01");
        assert_eq!(Payload::Bytes(vec![0x0c, 0x1a]).to_string(), "0c 1a");
    }

    #[test]
    fn restart() {
        assert_eq!(
            enc(&Tpdu::Connected {
                sequence: 0,
                apdu: Apdu::restart()
            }),
            [0x43, 0x80]
        );
        assert_eq!(
            Tpdu::decode(&[0x43, 0x80]),
            Ok(Tpdu::Connected {
                sequence: 0,
                apdu: Apdu::restart()
            })
        );
    }

    #[test]
    fn non_group_services_pass_through() {
        // A_PropertyValue_Read (APCI 0x3D5) with 4 data octets.
        let bytes = [0x03, 0xD5, 0x00, 0x0B, 0x10, 0x01];
        let tpdu = Tpdu::decode(&bytes).unwrap();
        assert!(matches!(tpdu, Tpdu::Data(Apdu::Other { apci: 0x3D5, .. })));
        assert_eq!(enc(&tpdu), bytes);
    }
}
