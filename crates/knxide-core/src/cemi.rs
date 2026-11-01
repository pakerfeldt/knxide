//! cEMI frames, the message format carried inside KNXnet/IP.
//!
//! Spec: 3/6/3 (EMI/IMI) §4.1.4 for message codes and the `L_Data` layout, and 3/2/2
//! (Communication Medium TP1) §2.2 for the control fields.

use alloc::vec::Vec;

use crate::address::{Destination, GroupAddress, IndividualAddress};
use crate::apdu::{Apdu, Tpdu};
use crate::error::{DecodeError, EncodeError};
use crate::reader::Reader;

/// cEMI message code `L_Data.req`: ask the interface to send a frame. Spec: 3/6/3 §4.1.4.1.
pub const L_DATA_REQ: u8 = 0x11;
/// cEMI message code `L_Data.con`: the interface's local confirmation of an `L_Data.req`.
pub const L_DATA_CON: u8 = 0x2E;
/// cEMI message code `L_Data.ind`: a frame received from the bus.
pub const L_DATA_IND: u8 = 0x29;
/// cEMI message code `L_Busmon.ind`: a raw frame seen in busmonitor mode.
pub const L_BUSMON_IND: u8 = 0x2B;

/// Most octets after the TPCI octet that fit in a standard frame. Longer NPDUs need the
/// extended frame format. Spec: 3/2/2 §2.2.4.
pub const STANDARD_FRAME_MAX_LENGTH: usize = 15;

/// A cEMI message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Cemi {
    /// `L_Data.req`, sent by the client.
    LDataReq(LData),
    /// `L_Data.con`, confirms an `L_Data.req`. [`LData::confirm_error`] tells whether it failed.
    LDataCon(LData),
    /// `L_Data.ind`, a frame from the bus.
    LDataInd(LData),
    /// Any other message code, raw (busmonitor, property services, …).
    Other {
        /// The message code.
        message_code: u8,
        /// Everything after the message code.
        data: Vec<u8>,
    },
}

impl Cemi {
    /// Decode a complete cEMI frame. Trailing octets are an error.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let code = r.u8()?;
        let ldata = match code {
            L_DATA_REQ => Self::LDataReq,
            L_DATA_CON => Self::LDataCon,
            L_DATA_IND => Self::LDataInd,
            _ => {
                return Ok(Self::Other {
                    message_code: code,
                    data: r.rest().to_vec(),
                });
            }
        };
        LData::decode(&mut r).map(ldata)
    }

    /// Append the encoded frame to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let (code, ldata) = match self {
            Self::LDataReq(l) => (L_DATA_REQ, l),
            Self::LDataCon(l) => (L_DATA_CON, l),
            Self::LDataInd(l) => (L_DATA_IND, l),
            Self::Other { message_code, data } => {
                out.push(*message_code);
                out.extend_from_slice(data);
                return Ok(());
            }
        };
        out.push(code);
        ldata.encode(out)
    }

    /// Encode into a new buffer.
    pub fn to_vec(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = Vec::new();
        self.encode(&mut out)?;
        Ok(out)
    }

    /// The `L_Data` frame, for any of the three `L_Data` message codes.
    pub fn ldata(&self) -> Option<&LData> {
        match self {
            Self::LDataReq(l) | Self::LDataCon(l) | Self::LDataInd(l) => Some(l),
            Self::Other { .. } => None,
        }
    }
}

/// Frame priority, Ctrl1 bits 3–2. Spec: 3/2/2 §2.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Priority {
    /// `00`, reserved for system management.
    System,
    /// `01`.
    Normal,
    /// `10`.
    Urgent,
    /// `11`, used for normal group communication.
    #[default]
    Low,
}

impl Priority {
    const fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0b00 => Self::System,
            0b01 => Self::Normal,
            0b10 => Self::Urgent,
            _ => Self::Low,
        }
    }

    const fn bits(self) -> u8 {
        match self {
            Self::System => 0b00,
            Self::Normal => 0b01,
            Self::Urgent => 0b10,
            Self::Low => 0b11,
        }
    }
}

/// Ctrl1 bit 7, frame type: 1 = standard, 0 = extended. Derived from the NPDU length when
/// encoding and ignored when decoding; receivers must tolerate either (3/6/3 §4.1.5.2.3).
const CTRL1_STANDARD_FRAME: u8 = 0x80;
/// Ctrl1 bit 5, set = do not repeat on error. Inverted relative to [`LData::repeat_on_error`].
const CTRL1_DO_NOT_REPEAT: u8 = 0x20;
/// Ctrl1 bit 4, set = normal broadcast, clear = system broadcast.
const CTRL1_BROADCAST: u8 = 0x10;
/// Ctrl1 bit 1, layer-2 acknowledge requested.
const CTRL1_ACK_REQUEST: u8 = 0x02;
/// Ctrl1 bit 0, in `L_Data.con`: set = error.
const CTRL1_CONFIRM_ERROR: u8 = 0x01;
/// Ctrl2 bit 7, destination is a group address.
const CTRL2_GROUP_ADDRESS: u8 = 0x80;

/// Hop count for new frames. Spec: 3/2/2 §2.2.5.3 (network layer parameter, default 6).
pub const DEFAULT_HOP_COUNT: u8 = 6;

/// An `L_Data` frame. Spec: 3/6/3 §4.1.4.3.
///
/// The frame type and destination address type bits are not stored: they follow from the
/// NPDU length and the [`Destination`] variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LData {
    /// Additional information block, raw. Usually empty; busmonitor and RF frames use it.
    pub additional_info: Vec<u8>,
    /// Source address. Send `0.0.0` in `L_Data.req`; the interface fills in its own.
    pub source: IndividualAddress,
    /// Destination address.
    pub destination: Destination,
    /// Frame priority.
    pub priority: Priority,
    /// Whether the medium repeats the frame on error (Ctrl1 bit 5 clear).
    pub repeat_on_error: bool,
    /// System broadcast rather than normal broadcast (Ctrl1 bit 4 clear).
    pub system_broadcast: bool,
    /// Layer-2 acknowledge requested.
    pub ack_request: bool,
    /// In `L_Data.con`: the frame could not be sent on the bus.
    pub confirm_error: bool,
    /// Hop count, 0–7.
    pub hop_count: u8,
    /// Extended frame format, Ctrl2 bits 3–0. `0` for everything except LTE frames.
    pub extended_frame_format: u8,
    /// Transport layer PDU.
    pub tpdu: Tpdu,
}

impl LData {
    /// A frame with the usual defaults (`ctrl1 = 0xBC`, `ctrl2 = 0xE0` for a group
    /// destination): low priority, no repeat, normal broadcast, hop count 6, source `0.0.0`.
    pub fn new(destination: impl Into<Destination>, tpdu: Tpdu) -> Self {
        Self {
            additional_info: Vec::new(),
            source: IndividualAddress::UNSET,
            destination: destination.into(),
            priority: Priority::Low,
            repeat_on_error: false,
            system_broadcast: false,
            ack_request: false,
            confirm_error: false,
            hop_count: DEFAULT_HOP_COUNT,
            extended_frame_format: 0,
            tpdu,
        }
    }

    /// A group service frame (`T_Data_Group`) to `destination`.
    pub fn group(destination: GroupAddress, apdu: Apdu) -> Self {
        Self::new(destination, Tpdu::Data(apdu))
    }

    /// The destination group address, if any.
    pub fn group_address(&self) -> Option<GroupAddress> {
        match self.destination {
            Destination::Group(ga) => Some(ga),
            Destination::Individual(_) => None,
        }
    }

    /// The APDU, if the TPDU is a data PDU.
    pub fn apdu(&self) -> Option<&Apdu> {
        self.tpdu.apdu()
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let add_len = r.u8()?;
        let additional_info = r.take(usize::from(add_len))?.to_vec();
        let ctrl1 = r.u8()?;
        let ctrl2 = r.u8()?;
        let source = IndividualAddress::from_raw(r.u16()?);
        let raw_dst = r.u16()?;
        let destination = if ctrl2 & CTRL2_GROUP_ADDRESS != 0 {
            Destination::Group(GroupAddress::from_raw(raw_dst))
        } else {
            Destination::Individual(IndividualAddress::from_raw(raw_dst))
        };
        let npdu_len = r.u8()?;
        let tpdu = Tpdu::decode(r.take(usize::from(npdu_len) + 1)?)?;
        if r.remaining() != 0 {
            return Err(DecodeError::Invalid("cEMI length"));
        }
        Ok(Self {
            additional_info,
            source,
            destination,
            priority: Priority::from_bits(ctrl1 >> 2),
            repeat_on_error: ctrl1 & CTRL1_DO_NOT_REPEAT == 0,
            system_broadcast: ctrl1 & CTRL1_BROADCAST == 0,
            ack_request: ctrl1 & CTRL1_ACK_REQUEST != 0,
            confirm_error: ctrl1 & CTRL1_CONFIRM_ERROR != 0,
            hop_count: (ctrl2 >> 4) & 0x07,
            extended_frame_format: ctrl2 & 0x0F,
            tpdu,
        })
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        if self.hop_count > 7 {
            return Err(EncodeError::OutOfRange("hop count"));
        }
        if self.extended_frame_format > 0x0F {
            return Err(EncodeError::OutOfRange("extended frame format"));
        }
        let add_len = u8::try_from(self.additional_info.len())
            .map_err(|_| EncodeError::TooLong("additional info"))?;

        let mut tpdu = Vec::new();
        self.tpdu.encode(&mut tpdu)?;
        let npdu_len = tpdu.len() - 1;
        let npdu_len_octet = u8::try_from(npdu_len).map_err(|_| EncodeError::TooLong("APDU"))?;

        let mut ctrl1 = self.priority.bits() << 2;
        if npdu_len <= STANDARD_FRAME_MAX_LENGTH {
            ctrl1 |= CTRL1_STANDARD_FRAME;
        }
        if !self.repeat_on_error {
            ctrl1 |= CTRL1_DO_NOT_REPEAT;
        }
        if !self.system_broadcast {
            ctrl1 |= CTRL1_BROADCAST;
        }
        if self.ack_request {
            ctrl1 |= CTRL1_ACK_REQUEST;
        }
        if self.confirm_error {
            ctrl1 |= CTRL1_CONFIRM_ERROR;
        }
        let (dst_flag, raw_dst) = match self.destination {
            Destination::Group(ga) => (CTRL2_GROUP_ADDRESS, ga.raw()),
            Destination::Individual(ia) => (0, ia.raw()),
        };
        let ctrl2 = dst_flag | self.hop_count << 4 | self.extended_frame_format;

        out.push(add_len);
        out.extend_from_slice(&self.additional_info);
        out.push(ctrl1);
        out.push(ctrl2);
        out.extend_from_slice(&self.source.raw().to_be_bytes());
        out.extend_from_slice(&raw_dst.to_be_bytes());
        out.push(npdu_len_octet);
        out.extend_from_slice(&tpdu);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apdu::Payload;
    use crate::ga;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn group_write_switch_on() {
        let frame = Cemi::LDataReq(LData::group(
            ga!(1 / 2 / 3),
            Apdu::GroupValueWrite(Payload::Small(1)),
        ));
        let bytes = frame.to_vec().unwrap();
        assert_eq!(
            bytes,
            [
                0x11, 0x00, 0xBC, 0xE0, 0x00, 0x00, 0x0A, 0x03, 0x01, 0x00, 0x81
            ]
        );
        assert_eq!(Cemi::decode(&bytes), Ok(frame));
    }

    #[test]
    fn indication_with_two_byte_value() {
        // L_Data.ind from 1.1.5 to 0/0/1 carrying a DPT 9 value: 21.5 °C = 0x0C33.
        let bytes = [
            0x29, 0x00, 0xBC, 0xD0, 0x11, 0x05, 0x00, 0x01, 0x03, 0x00, 0x80, 0x0C, 0x33,
        ];
        let Cemi::LDataInd(ldata) = Cemi::decode(&bytes).unwrap() else {
            panic!()
        };
        assert_eq!(ldata.source.to_string(), "1.1.5");
        assert_eq!(ldata.group_address(), Some(ga!(0 / 0 / 1)));
        assert_eq!(ldata.hop_count, 5);
        assert_eq!(ldata.priority, Priority::Low);
        assert_eq!(
            ldata.apdu(),
            Some(&Apdu::GroupValueWrite(Payload::Bytes(vec![0x0C, 0x33])))
        );
    }

    #[test]
    fn confirmation_error_flag() {
        let bytes = [
            0x2E, 0x00, 0xBD, 0xE0, 0x11, 0xFF, 0x0A, 0x03, 0x01, 0x00, 0x81,
        ];
        let Cemi::LDataCon(ldata) = Cemi::decode(&bytes).unwrap() else {
            panic!()
        };
        assert!(ldata.confirm_error);
    }

    #[test]
    fn additional_info_is_kept() {
        let bytes = [
            0x29, 0x03, 0x01, 0x02, 0x03, 0xBC, 0xE0, 0, 0, 0, 1, 0x01, 0x00, 0x81,
        ];
        let cemi = Cemi::decode(&bytes).unwrap();
        assert_eq!(cemi.ldata().unwrap().additional_info, [1, 2, 3]);
        assert_eq!(cemi.to_vec().unwrap(), bytes);
    }

    #[test]
    fn long_apdu_uses_extended_frame() {
        let frame = LData::group(
            ga!(0 / 0 / 1),
            Apdu::GroupValueWrite(Payload::Bytes(vec![0; 15])),
        );
        let bytes = Cemi::LDataReq(frame).to_vec().unwrap();
        assert_eq!(bytes[2] & CTRL1_STANDARD_FRAME, 0);
        assert_eq!(bytes[8], 16);
    }

    #[test]
    fn rejects_truncated_and_trailing() {
        let good = [0x29, 0x00, 0xBC, 0xE0, 0, 0, 0, 1, 0x01, 0x00, 0x81];
        for n in 0..good.len() {
            assert!(Cemi::decode(&good[..n]).is_err(), "prefix {n}");
        }
        let mut long = good.to_vec();
        long.push(0);
        assert_eq!(
            Cemi::decode(&long),
            Err(DecodeError::Invalid("cEMI length"))
        );
    }

    #[test]
    fn other_message_codes_pass_through() {
        let bytes = [0xFC, 0x00, 0x00, 0x01, 0x34, 0x10, 0x01];
        let cemi = Cemi::decode(&bytes).unwrap();
        assert!(matches!(
            cemi,
            Cemi::Other {
                message_code: 0xFC,
                ..
            }
        ));
        assert_eq!(cemi.to_vec().unwrap(), bytes);
    }
}
