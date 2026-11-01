//! KNXnet/IP frames: the 6-octet header plus one service body. Spec: 3/8/2 (Core) for the
//! header and core services, 3/8/4 (Tunnelling), 3/8/5 (Routing).

use std::fmt;

use knxide_core::reader::Reader;
use knxide_core::{DecodeError, EncodeError};

use crate::structures::{Crd, Cri, Dib, Hpai};

/// Header length octet. Spec: 3/8/2, frame header.
pub const HEADER_LEN: u8 = 0x06;
/// Protocol version 1.0.
pub const PROTOCOL_VERSION: u8 = 0x10;
/// UDP and TCP port for KNXnet/IP. Spec: 3/8/2.
pub const KNXNETIP_PORT: u16 = 3671;

/// Service type identifiers. Spec: 3/8/1 (Overview), service type number ranges.
pub mod service {
    /// `SEARCH_REQUEST`.
    pub const SEARCH_REQUEST: u16 = 0x0201;
    /// `SEARCH_RESPONSE`.
    pub const SEARCH_RESPONSE: u16 = 0x0202;
    /// `DESCRIPTION_REQUEST`.
    pub const DESCRIPTION_REQUEST: u16 = 0x0203;
    /// `DESCRIPTION_RESPONSE`.
    pub const DESCRIPTION_RESPONSE: u16 = 0x0204;
    /// `CONNECT_REQUEST`.
    pub const CONNECT_REQUEST: u16 = 0x0205;
    /// `CONNECT_RESPONSE`.
    pub const CONNECT_RESPONSE: u16 = 0x0206;
    /// `CONNECTIONSTATE_REQUEST`, the heartbeat.
    pub const CONNECTIONSTATE_REQUEST: u16 = 0x0207;
    /// `CONNECTIONSTATE_RESPONSE`.
    pub const CONNECTIONSTATE_RESPONSE: u16 = 0x0208;
    /// `DISCONNECT_REQUEST`.
    pub const DISCONNECT_REQUEST: u16 = 0x0209;
    /// `DISCONNECT_RESPONSE`.
    pub const DISCONNECT_RESPONSE: u16 = 0x020A;
    /// `SEARCH_REQUEST_EXTENDED`.
    pub const SEARCH_REQUEST_EXTENDED: u16 = 0x020B;
    /// `SEARCH_RESPONSE_EXTENDED`.
    pub const SEARCH_RESPONSE_EXTENDED: u16 = 0x020C;
    /// `TUNNELLING_REQUEST`.
    pub const TUNNELLING_REQUEST: u16 = 0x0420;
    /// `TUNNELLING_ACK`.
    pub const TUNNELLING_ACK: u16 = 0x0421;
    /// `ROUTING_INDICATION`.
    pub const ROUTING_INDICATION: u16 = 0x0530;
    /// `ROUTING_LOST_MESSAGE`.
    pub const ROUTING_LOST_MESSAGE: u16 = 0x0531;
    /// `ROUTING_BUSY`.
    pub const ROUTING_BUSY: u16 = 0x0532;
    /// `SECURE_WRAPPER` (KNX IP Secure).
    pub const SECURE_WRAPPER: u16 = 0x0950;
}

/// Status / error code carried by responses and acks. Spec: 3/8/2, error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Status(pub u8);

impl Status {
    /// `E_NO_ERROR`.
    pub const NO_ERROR: Self = Self(0x00);
    /// `E_HOST_PROTOCOL_TYPE`: the requested host protocol is not supported.
    pub const HOST_PROTOCOL_TYPE: Self = Self(0x01);
    /// `E_VERSION_NOT_SUPPORTED`.
    pub const VERSION_NOT_SUPPORTED: Self = Self(0x02);
    /// `E_SEQUENCE_NUMBER`: received sequence number out of order.
    pub const SEQUENCE_NUMBER: Self = Self(0x04);
    /// `E_ERROR`: unspecified error.
    pub const ERROR: Self = Self(0x0F);
    /// `E_CONNECTION_ID`: no active connection with this channel id.
    pub const CONNECTION_ID: Self = Self(0x21);
    /// `E_CONNECTION_TYPE`: connection type not supported.
    pub const CONNECTION_TYPE: Self = Self(0x22);
    /// `E_CONNECTION_OPTION`: connection option not supported.
    pub const CONNECTION_OPTION: Self = Self(0x23);
    /// `E_NO_MORE_CONNECTIONS`: all tunnels are in use.
    pub const NO_MORE_CONNECTIONS: Self = Self(0x24);
    /// `E_NO_MORE_UNIQUE_CONNECTIONS`.
    pub const NO_MORE_UNIQUE_CONNECTIONS: Self = Self(0x25);
    /// `E_DATA_CONNECTION`: error in the data connection.
    pub const DATA_CONNECTION: Self = Self(0x26);
    /// `E_KNX_CONNECTION`: error in the KNX subnetwork connection.
    pub const KNX_CONNECTION: Self = Self(0x27);
    /// `E_AUTHORISATION_ERROR`.
    pub const AUTHORISATION_ERROR: Self = Self(0x28);
    /// `E_TUNNELLING_LAYER`: tunnelling layer not supported.
    pub const TUNNELLING_LAYER: Self = Self(0x29);
    /// `E_NO_TUNNELLING_ADDRESS`: requested tunnel address unavailable.
    pub const NO_TUNNELLING_ADDRESS: Self = Self(0x2D);
    /// `E_CONNECTION_IN_USE`: requested tunnel address in use.
    pub const CONNECTION_IN_USE: Self = Self(0x2E);

    /// Whether this is [`Status::NO_ERROR`].
    pub fn is_ok(self) -> bool {
        self == Self::NO_ERROR
    }

    fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::NO_ERROR => "no error",
            Self::HOST_PROTOCOL_TYPE => "host protocol type not supported",
            Self::VERSION_NOT_SUPPORTED => "protocol version not supported",
            Self::SEQUENCE_NUMBER => "sequence number out of order",
            Self::ERROR => "unspecified error",
            Self::CONNECTION_ID => "unknown connection id",
            Self::CONNECTION_TYPE => "connection type not supported",
            Self::CONNECTION_OPTION => "connection option not supported",
            Self::NO_MORE_CONNECTIONS => "no more connections available",
            Self::NO_MORE_UNIQUE_CONNECTIONS => "no more unique connections available",
            Self::DATA_CONNECTION => "data connection error",
            Self::KNX_CONNECTION => "KNX connection error",
            Self::AUTHORISATION_ERROR => "authorisation error",
            Self::TUNNELLING_LAYER => "tunnelling layer not supported",
            Self::NO_TUNNELLING_ADDRESS => "requested tunnel address not available",
            Self::CONNECTION_IN_USE => "requested tunnel address in use",
            _ => return None,
        })
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => write!(f, "{name} ({:#04x})", self.0),
            None => write!(f, "status {:#04x}", self.0),
        }
    }
}

/// One KNXnet/IP frame.
///
/// Tunnelling and routing frames carry the cEMI frame as raw octets: a malformed cEMI must
/// not stop the tunnelling request from being acknowledged. Decode it with
/// [`knxide_core::Cemi::decode`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Frame {
    /// Look for servers. Sent to the multicast group or a single server.
    SearchRequest {
        /// Where responses go.
        discovery_endpoint: Hpai,
    },
    /// A server announcing itself.
    SearchResponse {
        /// The server's control endpoint.
        control_endpoint: Hpai,
        /// Device information and supported service families, at least.
        dibs: Vec<Dib>,
    },
    /// Ask one server to describe itself.
    DescriptionRequest {
        /// Where the response goes.
        control_endpoint: Hpai,
    },
    /// A server's description.
    DescriptionResponse {
        /// Device information and supported service families, at least.
        dibs: Vec<Dib>,
    },
    /// Open a connection (a tunnel).
    ConnectRequest {
        /// Our control endpoint.
        control_endpoint: Hpai,
        /// Our data endpoint.
        data_endpoint: Hpai,
        /// What kind of connection.
        cri: Cri,
    },
    /// Result of a connect request.
    ConnectResponse {
        /// Channel id assigned to the connection.
        channel_id: u8,
        /// Whether the connection was accepted.
        status: Status,
        /// The server's data endpoint. Present when `status` is OK.
        data_endpoint: Option<Hpai>,
        /// Connection details. Present when `status` is OK.
        crd: Option<Crd>,
    },
    /// Heartbeat.
    ConnectionStateRequest {
        /// Channel id.
        channel_id: u8,
        /// Our control endpoint.
        control_endpoint: Hpai,
    },
    /// Heartbeat response.
    ConnectionStateResponse {
        /// Channel id.
        channel_id: u8,
        /// Connection state.
        status: Status,
    },
    /// Close a connection. Either side may send it.
    DisconnectRequest {
        /// Channel id.
        channel_id: u8,
        /// Sender's control endpoint.
        control_endpoint: Hpai,
    },
    /// Close confirmation.
    DisconnectResponse {
        /// Channel id.
        channel_id: u8,
        /// Status.
        status: Status,
    },
    /// A cEMI frame over a tunnel.
    TunnellingRequest {
        /// Channel id.
        channel_id: u8,
        /// Sequence counter of the sender.
        sequence: u8,
        /// The cEMI frame, raw.
        cemi: Vec<u8>,
    },
    /// Acknowledges a tunnelling request.
    TunnellingAck {
        /// Channel id.
        channel_id: u8,
        /// Sequence counter being acknowledged.
        sequence: u8,
        /// Status.
        status: Status,
    },
    /// A cEMI frame multicast between routers.
    RoutingIndication {
        /// The cEMI frame, raw.
        cemi: Vec<u8>,
    },
    /// A router dropped frames.
    RoutingLostMessage {
        /// Device state; bit 0 = KNX fault, bit 1 = IP fault.
        device_state: u8,
        /// Number of frames lost.
        lost_messages: u16,
    },
    /// A router asks senders to slow down.
    RoutingBusy {
        /// Device state.
        device_state: u8,
        /// How long to pause, in milliseconds.
        wait_time_ms: u16,
        /// Control field, normally 0.
        control: u16,
    },
    /// Any other service, raw (including secure services until they are implemented).
    Other {
        /// Service type identifier.
        service_type: u16,
        /// Body after the header.
        body: Vec<u8>,
    },
}

/// Connection header length used by tunnelling request/ack. Spec: 3/8/4.
const CONNECTION_HEADER_LEN: u8 = 0x04;

/// The total frame length from a header, if `bytes` holds at least a valid header. Used to
/// split a TCP stream into frames.
pub fn frame_len(bytes: &[u8]) -> Result<Option<usize>, DecodeError> {
    if bytes.len() < usize::from(HEADER_LEN) {
        return Ok(None);
    }
    let mut r = Reader::new(bytes);
    let (_, total_len) = decode_header(&mut r)?;
    Ok(Some(total_len))
}

fn decode_header(r: &mut Reader<'_>) -> Result<(u16, usize), DecodeError> {
    if r.u8()? != HEADER_LEN {
        return Err(DecodeError::Invalid("header length"));
    }
    if r.u8()? != PROTOCOL_VERSION {
        return Err(DecodeError::Invalid("protocol version"));
    }
    let service_type = r.u16()?;
    let total_len = usize::from(r.u16()?);
    if total_len < usize::from(HEADER_LEN) {
        return Err(DecodeError::Invalid("total length"));
    }
    Ok((service_type, total_len))
}

impl Frame {
    /// Decode one frame. `bytes` must hold at least the length given in the header; anything
    /// after it is ignored.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let (service_type, total_len) = decode_header(&mut r)?;
        let body = r.take(total_len - usize::from(HEADER_LEN))?;
        let mut b = Reader::new(body);
        let frame = Self::decode_body(service_type, &mut b)?;
        if b.remaining() != 0 && !matches!(frame, Self::Other { .. }) {
            return Err(DecodeError::Invalid("body length"));
        }
        Ok(frame)
    }

    fn decode_body(service_type: u16, b: &mut Reader<'_>) -> Result<Self, DecodeError> {
        use service::*;
        Ok(match service_type {
            SEARCH_REQUEST => Self::SearchRequest {
                discovery_endpoint: Hpai::decode(b)?,
            },
            SEARCH_RESPONSE => Self::SearchResponse {
                control_endpoint: Hpai::decode(b)?,
                dibs: Dib::decode_all(b)?,
            },
            DESCRIPTION_REQUEST => Self::DescriptionRequest {
                control_endpoint: Hpai::decode(b)?,
            },
            DESCRIPTION_RESPONSE => Self::DescriptionResponse {
                dibs: Dib::decode_all(b)?,
            },
            CONNECT_REQUEST => Self::ConnectRequest {
                control_endpoint: Hpai::decode(b)?,
                data_endpoint: Hpai::decode(b)?,
                cri: Cri::decode(b)?,
            },
            CONNECT_RESPONSE => {
                let channel_id = b.u8()?;
                let status = Status(b.u8()?);
                let (data_endpoint, crd) = if status.is_ok() {
                    (Some(Hpai::decode(b)?), Some(Crd::decode(b)?))
                } else {
                    // Some servers pad error responses; nothing after the status is meaningful.
                    b.rest();
                    (None, None)
                };
                Self::ConnectResponse {
                    channel_id,
                    status,
                    data_endpoint,
                    crd,
                }
            }
            CONNECTIONSTATE_REQUEST => {
                let channel_id = b.u8()?;
                let _reserved = b.u8()?;
                Self::ConnectionStateRequest {
                    channel_id,
                    control_endpoint: Hpai::decode(b)?,
                }
            }
            CONNECTIONSTATE_RESPONSE => Self::ConnectionStateResponse {
                channel_id: b.u8()?,
                status: Status(b.u8()?),
            },
            DISCONNECT_REQUEST => {
                let channel_id = b.u8()?;
                let _reserved = b.u8()?;
                Self::DisconnectRequest {
                    channel_id,
                    control_endpoint: Hpai::decode(b)?,
                }
            }
            DISCONNECT_RESPONSE => Self::DisconnectResponse {
                channel_id: b.u8()?,
                status: Status(b.u8()?),
            },
            TUNNELLING_REQUEST => {
                let (channel_id, sequence, _reserved) = decode_connection_header(b)?;
                Self::TunnellingRequest {
                    channel_id,
                    sequence,
                    cemi: b.rest().to_vec(),
                }
            }
            TUNNELLING_ACK => {
                let (channel_id, sequence, status) = decode_connection_header(b)?;
                Self::TunnellingAck {
                    channel_id,
                    sequence,
                    status: Status(status),
                }
            }
            ROUTING_INDICATION => Self::RoutingIndication {
                cemi: b.rest().to_vec(),
            },
            ROUTING_LOST_MESSAGE => {
                if b.u8()? != 4 {
                    return Err(DecodeError::Invalid("lost message info length"));
                }
                Self::RoutingLostMessage {
                    device_state: b.u8()?,
                    lost_messages: b.u16()?,
                }
            }
            ROUTING_BUSY => {
                if b.u8()? != 6 {
                    return Err(DecodeError::Invalid("busy info length"));
                }
                Self::RoutingBusy {
                    device_state: b.u8()?,
                    wait_time_ms: b.u16()?,
                    control: b.u16()?,
                }
            }
            _ => Self::Other {
                service_type,
                body: b.rest().to_vec(),
            },
        })
    }

    /// The service type identifier.
    pub fn service_type(&self) -> u16 {
        use service::*;
        match self {
            Self::SearchRequest { .. } => SEARCH_REQUEST,
            Self::SearchResponse { .. } => SEARCH_RESPONSE,
            Self::DescriptionRequest { .. } => DESCRIPTION_REQUEST,
            Self::DescriptionResponse { .. } => DESCRIPTION_RESPONSE,
            Self::ConnectRequest { .. } => CONNECT_REQUEST,
            Self::ConnectResponse { .. } => CONNECT_RESPONSE,
            Self::ConnectionStateRequest { .. } => CONNECTIONSTATE_REQUEST,
            Self::ConnectionStateResponse { .. } => CONNECTIONSTATE_RESPONSE,
            Self::DisconnectRequest { .. } => DISCONNECT_REQUEST,
            Self::DisconnectResponse { .. } => DISCONNECT_RESPONSE,
            Self::TunnellingRequest { .. } => TUNNELLING_REQUEST,
            Self::TunnellingAck { .. } => TUNNELLING_ACK,
            Self::RoutingIndication { .. } => ROUTING_INDICATION,
            Self::RoutingLostMessage { .. } => ROUTING_LOST_MESSAGE,
            Self::RoutingBusy { .. } => ROUTING_BUSY,
            Self::Other { service_type, .. } => *service_type,
        }
    }

    /// Append the encoded frame to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let start = out.len();
        out.extend_from_slice(&[HEADER_LEN, PROTOCOL_VERSION]);
        out.extend_from_slice(&self.service_type().to_be_bytes());
        out.extend_from_slice(&[0, 0]); // total length, patched below
        self.encode_body(out)?;
        let total = u16::try_from(out.len() - start).map_err(|_| EncodeError::TooLong("frame"))?;
        out.splice(start + 4..start + 6, total.to_be_bytes());
        Ok(())
    }

    /// Encode into a new buffer.
    pub fn to_vec(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = Vec::new();
        self.encode(&mut out)?;
        Ok(out)
    }

    fn encode_body(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        match self {
            Self::SearchRequest {
                discovery_endpoint: hpai,
            }
            | Self::DescriptionRequest {
                control_endpoint: hpai,
            } => hpai.encode(out),
            Self::SearchResponse {
                control_endpoint,
                dibs,
            } => {
                control_endpoint.encode(out);
                for dib in dibs {
                    dib.encode(out)?;
                }
            }
            Self::DescriptionResponse { dibs } => {
                for dib in dibs {
                    dib.encode(out)?;
                }
            }
            Self::ConnectRequest {
                control_endpoint,
                data_endpoint,
                cri,
            } => {
                control_endpoint.encode(out);
                data_endpoint.encode(out);
                cri.encode(out)?;
            }
            Self::ConnectResponse {
                channel_id,
                status,
                data_endpoint,
                crd,
            } => {
                out.extend_from_slice(&[*channel_id, status.0]);
                if let Some(hpai) = data_endpoint {
                    hpai.encode(out);
                }
                if let Some(crd) = crd {
                    crd.encode(out)?;
                }
            }
            Self::ConnectionStateRequest {
                channel_id,
                control_endpoint,
            }
            | Self::DisconnectRequest {
                channel_id,
                control_endpoint,
            } => {
                out.extend_from_slice(&[*channel_id, 0x00]);
                control_endpoint.encode(out);
            }
            Self::ConnectionStateResponse { channel_id, status }
            | Self::DisconnectResponse { channel_id, status } => {
                out.extend_from_slice(&[*channel_id, status.0]);
            }
            Self::TunnellingRequest {
                channel_id,
                sequence,
                cemi,
            } => {
                out.extend_from_slice(&[CONNECTION_HEADER_LEN, *channel_id, *sequence, 0x00]);
                out.extend_from_slice(cemi);
            }
            Self::TunnellingAck {
                channel_id,
                sequence,
                status,
            } => {
                out.extend_from_slice(&[CONNECTION_HEADER_LEN, *channel_id, *sequence, status.0]);
            }
            Self::RoutingIndication { cemi } => out.extend_from_slice(cemi),
            Self::RoutingLostMessage {
                device_state,
                lost_messages,
            } => {
                out.extend_from_slice(&[4, *device_state]);
                out.extend_from_slice(&lost_messages.to_be_bytes());
            }
            Self::RoutingBusy {
                device_state,
                wait_time_ms,
                control,
            } => {
                out.extend_from_slice(&[6, *device_state]);
                out.extend_from_slice(&wait_time_ms.to_be_bytes());
                out.extend_from_slice(&control.to_be_bytes());
            }
            Self::Other { body, .. } => out.extend_from_slice(body),
        }
        Ok(())
    }
}

fn decode_connection_header(b: &mut Reader<'_>) -> Result<(u8, u8, u8), DecodeError> {
    if b.u8()? != CONNECTION_HEADER_LEN {
        return Err(DecodeError::Invalid("connection header length"));
    }
    Ok((b.u8()?, b.u8()?, b.u8()?))
}
