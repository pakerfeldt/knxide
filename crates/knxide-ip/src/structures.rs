//! Structures shared by KNXnet/IP services: HPAI, CRI/CRD and DIBs. Spec: 3/8/2 (Core).

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use knxide_core::reader::Reader;
use knxide_core::{DecodeError, EncodeError, IndividualAddress};

fn struct_len(len: usize, what: &'static str) -> Result<u8, EncodeError> {
    u8::try_from(len).map_err(|_| EncodeError::TooLong(what))
}

/// Read a structure that starts with its own length octet. Returns the octets after the length.
fn length_prefixed<'a>(r: &mut Reader<'a>, min: u8) -> Result<&'a [u8], DecodeError> {
    let len = r.u8()?;
    if len < min {
        return Err(DecodeError::Invalid("structure length"));
    }
    r.take(usize::from(len) - 1)
}

/// Host protocol of an [`Hpai`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostProtocol {
    /// `0x01`, IPv4 over UDP.
    Udp,
    /// `0x02`, IPv4 over TCP.
    Tcp,
}

/// Host Protocol Address Information: an IPv4 endpoint. Spec: 3/8/2, HPAI.
///
/// `0.0.0.0:0` over UDP means "route back": reply to the source address of the datagram.
/// That is the default here because it works through NAT and container networking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hpai {
    /// UDP or TCP.
    pub protocol: HostProtocol,
    /// Address and port.
    pub address: SocketAddrV4,
}

impl Hpai {
    /// Encoded length.
    pub const LEN: u8 = 8;

    /// The UDP route-back endpoint `0.0.0.0:0`.
    pub const ROUTE_BACK: Self = Self::udp(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));

    /// A UDP endpoint.
    pub const fn udp(address: SocketAddrV4) -> Self {
        Self {
            protocol: HostProtocol::Udp,
            address,
        }
    }

    /// The TCP endpoint `0.0.0.0:0`; over TCP the connection itself is the endpoint.
    pub const TCP: Self = Self {
        protocol: HostProtocol::Tcp,
        address: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
    };

    /// Whether this is a route-back (NAT) endpoint: address or port unset.
    pub fn is_route_back(&self) -> bool {
        self.address.ip().is_unspecified() || self.address.port() == 0
    }

    /// Resolve the endpoint to send to: the HPAI itself, or `source` for route-back.
    pub fn resolve(&self, source: SocketAddr) -> SocketAddr {
        if self.is_route_back() {
            source
        } else {
            SocketAddr::V4(self.address)
        }
    }

    pub(crate) fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let body = length_prefixed(r, Self::LEN)?;
        let mut b = Reader::new(body);
        let protocol = match b.u8()? {
            0x01 => HostProtocol::Udp,
            0x02 => HostProtocol::Tcp,
            _ => return Err(DecodeError::Invalid("HPAI protocol")),
        };
        let ip = Ipv4Addr::from(b.array::<4>()?);
        let port = b.u16()?;
        Ok(Self {
            protocol,
            address: SocketAddrV4::new(ip, port),
        })
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        out.push(Self::LEN);
        out.push(match self.protocol {
            HostProtocol::Udp => 0x01,
            HostProtocol::Tcp => 0x02,
        });
        out.extend_from_slice(&self.address.ip().octets());
        out.extend_from_slice(&self.address.port().to_be_bytes());
    }
}

/// Connection type `TUNNEL_CONNECTION`. Spec: 3/8/4 (Tunnelling), CRI.
pub const TUNNEL_CONNECTION: u8 = 0x04;

/// KNX layer requested for a tunnel. Spec: 3/8/4, CRI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TunnelLayer {
    /// `0x02`, data link layer: normal group communication.
    LinkLayer,
    /// `0x04`, raw frames.
    Raw,
    /// `0x80`, busmonitor: receive everything, send nothing.
    Busmonitor,
    /// Anything else.
    Other(u8),
}

impl TunnelLayer {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0x02 => Self::LinkLayer,
            0x04 => Self::Raw,
            0x80 => Self::Busmonitor,
            other => Self::Other(other),
        }
    }

    fn raw(self) -> u8 {
        match self {
            Self::LinkLayer => 0x02,
            Self::Raw => 0x04,
            Self::Busmonitor => 0x80,
            Self::Other(raw) => raw,
        }
    }
}

/// Connection Request Information, sent in `CONNECT_REQUEST`. Spec: 3/8/2 and 3/8/4, CRI.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Cri {
    /// A tunnel connection.
    Tunnel {
        /// Requested KNX layer.
        layer: TunnelLayer,
        /// Tunnelling v2: ask for a specific tunnel address (extended CRI).
        address: Option<IndividualAddress>,
    },
    /// Any other connection type, raw.
    Other {
        /// Connection type code.
        connection_type: u8,
        /// Octets after the connection type.
        data: Vec<u8>,
    },
}

impl Cri {
    /// A link-layer tunnel, the normal choice.
    pub const LINK_LAYER_TUNNEL: Self = Self::Tunnel {
        layer: TunnelLayer::LinkLayer,
        address: None,
    };

    pub(crate) fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let body = length_prefixed(r, 2)?;
        let mut b = Reader::new(body);
        let connection_type = b.u8()?;
        if connection_type != TUNNEL_CONNECTION {
            return Ok(Self::Other {
                connection_type,
                data: b.rest().to_vec(),
            });
        }
        let layer = TunnelLayer::from_raw(b.u8()?);
        let _reserved = b.u8()?;
        let address = match b.remaining() {
            0 => None,
            2 => Some(IndividualAddress::from_raw(b.u16()?)),
            _ => return Err(DecodeError::Invalid("CRI length")),
        };
        Ok(Self::Tunnel { layer, address })
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        match self {
            Self::Tunnel { layer, address } => {
                out.push(if address.is_some() { 6 } else { 4 });
                out.extend_from_slice(&[TUNNEL_CONNECTION, layer.raw(), 0x00]);
                if let Some(address) = address {
                    out.extend_from_slice(&address.raw().to_be_bytes());
                }
            }
            Self::Other {
                connection_type,
                data,
            } => {
                out.push(struct_len(data.len() + 2, "CRI")?);
                out.push(*connection_type);
                out.extend_from_slice(data);
            }
        }
        Ok(())
    }
}

/// Connection Response Data, returned in `CONNECT_RESPONSE`. Spec: 3/8/2 and 3/8/4, CRD.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Crd {
    /// A tunnel connection and the individual address assigned to it.
    Tunnel {
        /// The tunnel's individual address.
        address: IndividualAddress,
    },
    /// Any other connection type, raw.
    Other {
        /// Connection type code.
        connection_type: u8,
        /// Octets after the connection type.
        data: Vec<u8>,
    },
}

impl Crd {
    pub(crate) fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let body = length_prefixed(r, 2)?;
        let mut b = Reader::new(body);
        let connection_type = b.u8()?;
        if connection_type == TUNNEL_CONNECTION && b.remaining() == 2 {
            return Ok(Self::Tunnel {
                address: IndividualAddress::from_raw(b.u16()?),
            });
        }
        Ok(Self::Other {
            connection_type,
            data: b.rest().to_vec(),
        })
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        match self {
            Self::Tunnel { address } => {
                out.extend_from_slice(&[4, TUNNEL_CONNECTION]);
                out.extend_from_slice(&address.raw().to_be_bytes());
            }
            Self::Other {
                connection_type,
                data,
            } => {
                out.push(struct_len(data.len() + 2, "CRD")?);
                out.push(*connection_type);
                out.extend_from_slice(data);
            }
        }
        Ok(())
    }
}

/// KNX medium code, used in [`DeviceInfo`]. Spec: 3/8/2, Device Information DIB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Medium(pub u8);

impl Medium {
    /// Twisted pair, `0x02`.
    pub const TP1: Self = Self(0x02);
    /// Powerline 110 kHz, `0x04`.
    pub const PL110: Self = Self(0x04);
    /// Radio frequency, `0x10`.
    pub const RF: Self = Self(0x10);
    /// KNX IP, `0x20`.
    pub const IP: Self = Self(0x20);
}

impl fmt::Display for Medium {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TP1 => f.write_str("TP1"),
            Self::PL110 => f.write_str("PL110"),
            Self::RF => f.write_str("RF"),
            Self::IP => f.write_str("IP"),
            Self(other) => write!(f, "medium {other:#04x}"),
        }
    }
}

/// Service family ids. Spec: 3/8/2, Supported Service Families DIB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServiceFamily {
    /// Family id, e.g. [`ServiceFamily::TUNNELLING`].
    pub id: u8,
    /// Version of the family the device implements.
    pub version: u8,
}

impl ServiceFamily {
    /// KNXnet/IP Core.
    pub const CORE: u8 = 0x02;
    /// Device Management.
    pub const DEVICE_MANAGEMENT: u8 = 0x03;
    /// Tunnelling.
    pub const TUNNELLING: u8 = 0x04;
    /// Routing.
    pub const ROUTING: u8 = 0x05;
    /// Remote Logging.
    pub const REMOTE_LOGGING: u8 = 0x06;
    /// Remote Configuration and Diagnosis.
    pub const REMOTE_CONFIGURATION: u8 = 0x07;
    /// Object Server.
    pub const OBJECT_SERVER: u8 = 0x08;
    /// Security.
    pub const SECURITY: u8 = 0x09;

    /// Family name, if known.
    pub fn name(&self) -> Option<&'static str> {
        Some(match self.id {
            Self::CORE => "core",
            Self::DEVICE_MANAGEMENT => "device management",
            Self::TUNNELLING => "tunnelling",
            Self::ROUTING => "routing",
            Self::REMOTE_LOGGING => "remote logging",
            Self::REMOTE_CONFIGURATION => "remote configuration",
            Self::OBJECT_SERVER => "object server",
            Self::SECURITY => "security",
            _ => return None,
        })
    }
}

/// Device Information DIB contents (type `0x01`, 54 octets). Spec: 3/8/2, DIB.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceInfo {
    /// KNX medium the device is on.
    pub medium: Medium,
    /// Device status; bit 0 = programming mode.
    pub status: u8,
    /// The device's own individual address.
    pub address: IndividualAddress,
    /// Project-installation identifier.
    pub project_installation_id: u16,
    /// KNX serial number.
    pub serial_number: [u8; 6],
    /// Routing multicast address (`0.0.0.0` if the device does not route).
    pub multicast_address: Ipv4Addr,
    /// MAC address.
    pub mac_address: [u8; 6],
    /// Friendly name, ISO 8859-1, at most 30 characters.
    pub name: String,
}

impl DeviceInfo {
    const LEN: u8 = 54;
    const NAME_LEN: usize = 30;

    /// Whether the device is in programming mode.
    pub fn programming_mode(&self) -> bool {
        self.status & 0x01 != 0
    }

    fn decode(b: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let medium = Medium(b.u8()?);
        let status = b.u8()?;
        let address = IndividualAddress::from_raw(b.u16()?);
        let project_installation_id = b.u16()?;
        let serial_number = b.array()?;
        let multicast_address = Ipv4Addr::from(b.array::<4>()?);
        let mac_address = b.array()?;
        let raw_name = b.take(Self::NAME_LEN)?;
        let name = raw_name
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| char::from(c))
            .collect();
        Ok(Self {
            medium,
            status,
            address,
            project_installation_id,
            serial_number,
            multicast_address,
            mac_address,
            name,
        })
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        let mut name = Vec::with_capacity(Self::NAME_LEN);
        for ch in self.name.chars() {
            name.push(
                u8::try_from(u32::from(ch)).map_err(|_| EncodeError::OutOfRange("device name"))?,
            );
        }
        if name.len() > Self::NAME_LEN {
            return Err(EncodeError::TooLong("device name"));
        }
        name.resize(Self::NAME_LEN, 0);
        out.extend_from_slice(&[Self::LEN, Dib::DEVICE_INFO, self.medium.0, self.status]);
        out.extend_from_slice(&self.address.raw().to_be_bytes());
        out.extend_from_slice(&self.project_installation_id.to_be_bytes());
        out.extend_from_slice(&self.serial_number);
        out.extend_from_slice(&self.multicast_address.octets());
        out.extend_from_slice(&self.mac_address);
        out.extend_from_slice(&name);
        Ok(())
    }
}

/// Description Information Block. Spec: 3/8/2, DIB.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Dib {
    /// Type `0x01`.
    DeviceInfo(DeviceInfo),
    /// Type `0x02`.
    SupportedServiceFamilies(Vec<ServiceFamily>),
    /// Any other type, raw. Decoders skip DIBs they do not understand.
    Other {
        /// DIB type code.
        dib_type: u8,
        /// Octets after the type code.
        data: Vec<u8>,
    },
}

impl Dib {
    /// DIB type code of [`Dib::DeviceInfo`].
    pub const DEVICE_INFO: u8 = 0x01;
    /// DIB type code of [`Dib::SupportedServiceFamilies`].
    pub const SUPPORTED_SERVICE_FAMILIES: u8 = 0x02;

    /// Decode DIBs until the input is exhausted.
    pub(crate) fn decode_all(r: &mut Reader<'_>) -> Result<Vec<Self>, DecodeError> {
        let mut dibs = Vec::new();
        while r.remaining() > 0 {
            dibs.push(Self::decode(r)?);
        }
        Ok(dibs)
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let body = length_prefixed(r, 2)?;
        let mut b = Reader::new(body);
        let dib_type = b.u8()?;
        let dib = match dib_type {
            Self::DEVICE_INFO if body.len() == usize::from(DeviceInfo::LEN) - 1 => {
                Self::DeviceInfo(DeviceInfo::decode(&mut b)?)
            }
            Self::SUPPORTED_SERVICE_FAMILIES if b.remaining().is_multiple_of(2) => {
                let mut families = Vec::new();
                while b.remaining() > 0 {
                    families.push(ServiceFamily {
                        id: b.u8()?,
                        version: b.u8()?,
                    });
                }
                Self::SupportedServiceFamilies(families)
            }
            _ => Self::Other {
                dib_type,
                data: b.rest().to_vec(),
            },
        };
        Ok(dib)
    }

    pub(crate) fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        match self {
            Self::DeviceInfo(info) => info.encode(out)?,
            Self::SupportedServiceFamilies(families) => {
                out.push(struct_len(families.len() * 2 + 2, "service families DIB")?);
                out.push(Self::SUPPORTED_SERVICE_FAMILIES);
                for family in families {
                    out.extend_from_slice(&[family.id, family.version]);
                }
            }
            Self::Other { dib_type, data } => {
                out.push(struct_len(data.len() + 2, "DIB")?);
                out.push(*dib_type);
                out.extend_from_slice(data);
            }
        }
        Ok(())
    }
}

/// Convenience accessors over the DIBs of a search or description response.
pub trait Dibs {
    /// The device information DIB, if present.
    fn device_info(&self) -> Option<&DeviceInfo>;
    /// The supported service families, if present.
    fn service_families(&self) -> &[ServiceFamily];
    /// Whether a service family is supported.
    fn supports(&self, family: u8) -> bool {
        self.service_families().iter().any(|f| f.id == family)
    }
}

impl Dibs for [Dib] {
    fn device_info(&self) -> Option<&DeviceInfo> {
        self.iter().find_map(|d| match d {
            Dib::DeviceInfo(info) => Some(info),
            _ => None,
        })
    }

    fn service_families(&self) -> &[ServiceFamily] {
        self.iter()
            .find_map(|d| match d {
                Dib::SupportedServiceFamilies(f) => Some(f.as_slice()),
                _ => None,
            })
            .unwrap_or(&[])
    }
}
