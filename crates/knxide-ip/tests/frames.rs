//! KNXnet/IP frame fixtures and property tests.
//!
//! Fixture bytes come from xknx's test suite (MIT), test/knxip_tests/.

use std::net::{Ipv4Addr, SocketAddrV4};

use knxide_core::{Cemi, DecodeError, IndividualAddress};
use knxide_ip::frame::frame_len;
use knxide_ip::structures::{HostProtocol, Medium, TunnelLayer};
use knxide_ip::{Crd, Cri, Dib, Dibs, Frame, Hpai, ServiceFamily, Status};
use proptest::prelude::*;

fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

fn hpai(a: [u8; 4], port: u16) -> Hpai {
    Hpai::udp(SocketAddrV4::new(Ipv4Addr::from(a), port))
}

/// Decodes to `frame` and re-encodes to the same bytes.
fn golden(bytes: &[u8], frame: &Frame) {
    assert_eq!(Frame::decode(bytes).as_ref(), Ok(frame));
    assert_eq!(frame.to_vec().unwrap(), bytes);
}

#[test]
fn connect_request() {
    golden(
        &hex("06 10 02 05 00 1A 08 01 C0 A8 2A 01 84 95 08 01 C0 A8 2A 01 CC A9 04 04 02 00"),
        &Frame::ConnectRequest {
            control_endpoint: hpai([192, 168, 42, 1], 33941),
            data_endpoint: hpai([192, 168, 42, 1], 52393),
            cri: Cri::LINK_LAYER_TUNNEL,
        },
    );
    // Tunnelling v2 extended CRI asking for tunnel address 1.1.255.
    golden(
        &hex("06 10 02 05 00 1C 08 01 C0 A8 2A 01 84 95 08 01 C0 A8 2A 01 CC A9 06 04 02 00 11 FF"),
        &Frame::ConnectRequest {
            control_endpoint: hpai([192, 168, 42, 1], 33941),
            data_endpoint: hpai([192, 168, 42, 1], 52393),
            cri: Cri::Tunnel {
                layer: TunnelLayer::LinkLayer,
                address: Some(IndividualAddress::from_raw(0x11FF)),
            },
        },
    );
    // Device management connection: passed through raw.
    golden(
        &hex("06 10 02 05 00 18 08 01 C0 A8 2A 01 84 95 08 01 C0 A8 2A 01 CC A9 02 03"),
        &Frame::ConnectRequest {
            control_endpoint: hpai([192, 168, 42, 1], 33941),
            data_endpoint: hpai([192, 168, 42, 1], 52393),
            cri: Cri::Other {
                connection_type: 0x03,
                data: vec![],
            },
        },
    );
}

#[test]
fn connect_response() {
    golden(
        &hex("06 10 02 06 00 14 01 00 08 01 C0 A8 2A 0A 0E 57 04 04 11 FF"),
        &Frame::ConnectResponse {
            channel_id: 1,
            status: Status::NO_ERROR,
            data_endpoint: Some(hpai([192, 168, 42, 10], 3671)),
            crd: Some(Crd::Tunnel {
                address: IndividualAddress::from_raw(0x11FF),
            }),
        },
    );
    golden(
        &hex("06 10 02 06 00 08 00 24"),
        &Frame::ConnectResponse {
            channel_id: 0,
            status: Status::NO_MORE_CONNECTIONS,
            data_endpoint: None,
            crd: None,
        },
    );
}

#[test]
fn tunnelling() {
    let bytes = hex("06 10 04 20 00 15 04 01 17 00 11 00 BC E0 00 00 48 08 01 00 81");
    let frame = Frame::decode(&bytes).unwrap();
    let Frame::TunnellingRequest {
        channel_id: 1,
        sequence: 23,
        cemi,
    } = &frame
    else {
        panic!("{frame:?}")
    };
    let cemi = Cemi::decode(cemi).unwrap();
    assert_eq!(
        cemi.ldata().unwrap().group_address().unwrap().to_string(),
        "9/0/8"
    );
    assert_eq!(frame.to_vec().unwrap(), bytes);

    golden(
        &hex("06 10 04 21 00 0A 04 2A 17 00"),
        &Frame::TunnellingAck {
            channel_id: 42,
            sequence: 23,
            status: Status::NO_ERROR,
        },
    );
    assert_eq!(
        Frame::decode(&hex("06 10 04 21 00 0A 03 2A 17 00")),
        Err(DecodeError::Invalid("connection header length"))
    );
    // Header says 0x15 octets but only 7 arrived.
    assert_eq!(
        Frame::decode(&hex("06 10 04 20 00 15 04")),
        Err(DecodeError::UnexpectedEnd)
    );
}

#[test]
fn heartbeat_and_disconnect() {
    let endpoint = hpai([192, 168, 200, 12], 50100);
    golden(
        &hex("06 10 02 07 00 10 15 00 08 01 C0 A8 C8 0C C3 B4"),
        &Frame::ConnectionStateRequest {
            channel_id: 21,
            control_endpoint: endpoint,
        },
    );
    golden(
        &hex("06 10 02 08 00 08 15 21"),
        &Frame::ConnectionStateResponse {
            channel_id: 21,
            status: Status::CONNECTION_ID,
        },
    );
    golden(
        &hex("06 10 02 09 00 10 15 00 08 01 C0 A8 C8 0C C3 B4"),
        &Frame::DisconnectRequest {
            channel_id: 21,
            control_endpoint: endpoint,
        },
    );
    golden(
        &hex("06 10 02 0A 00 08 15 00"),
        &Frame::DisconnectResponse {
            channel_id: 21,
            status: Status::NO_ERROR,
        },
    );
}

#[test]
fn search_response() {
    let bytes = hex("06 10 02 02 00 50 08 01 C0 A8 2A 0A 0E 57 36 01
         02 00 11 00 00 00 11 22 33 44 55 66 E0 00 17 0C
         01 02 03 04 05 06 47 69 72 61 20 4B 4E 58 2F 49
         50 2D 52 6F 75 74 65 72 00 00 00 00 00 00 00 00
         00 00 00 00 0C 02 02 01 03 02 04 01 05 01 07 01");
    let frame = Frame::decode(&bytes).unwrap();
    let Frame::SearchResponse {
        control_endpoint,
        dibs,
    } = &frame
    else {
        panic!("{frame:?}")
    };
    assert_eq!(*control_endpoint, hpai([192, 168, 42, 10], 3671));
    let info = dibs.device_info().unwrap();
    assert_eq!(info.name, "Gira KNX/IP-Router");
    assert_eq!(info.medium, Medium::TP1);
    assert_eq!(info.address.to_string(), "1.1.0");
    assert_eq!(info.multicast_address, Ipv4Addr::new(224, 0, 23, 12));
    assert_eq!(info.mac_address, [1, 2, 3, 4, 5, 6]);
    assert!(dibs.supports(ServiceFamily::TUNNELLING));
    assert!(dibs.supports(ServiceFamily::ROUTING));
    assert!(!dibs.supports(ServiceFamily::SECURITY));
    assert_eq!(frame.to_vec().unwrap(), bytes);
}

#[test]
fn description_response_skips_unknown_dibs() {
    let mut bytes = hex("06 10 02 04 00 48 36 01 02 00 10 00 00 00 00 08
         2d 40 83 4d e0 00 17 0c 00 0a b3 27 4a 32 4b 4e
         58 2f 49 50 2d 52 6f 75 74 65 72 00 00 00 00 00
         00 00 00 00 00 00 00 00 00 00 00 00 0c 02 02 02
         03 02 04 02 05 02 07 01");
    // Append a manufacturer DIB (type 0xFE) and fix the total length.
    bytes.extend_from_slice(&[0x04, 0xFE, 0x00, 0x01]);
    bytes[5] += 4;
    let frame = Frame::decode(&bytes).unwrap();
    let Frame::DescriptionResponse { dibs } = &frame else {
        panic!("{frame:?}")
    };
    assert_eq!(dibs.device_info().unwrap().name, "KNX/IP-Router");
    assert_eq!(
        dibs.last(),
        Some(&Dib::Other {
            dib_type: 0xFE,
            data: vec![0, 1]
        })
    );
    assert_eq!(frame.to_vec().unwrap(), bytes);
}

#[test]
fn routing() {
    golden(
        &hex("06 10 05 32 00 0C 06 00 00 64 00 00"),
        &Frame::RoutingBusy {
            device_state: 0,
            wait_time_ms: 100,
            control: 0,
        },
    );
    golden(
        &hex("06 10 05 31 00 0A 04 00 00 05"),
        &Frame::RoutingLostMessage {
            device_state: 0,
            lost_messages: 5,
        },
    );
    assert_eq!(
        Frame::decode(&hex("06 10 05 32 00 0C 08 00 00 64 00 00")),
        Err(DecodeError::Invalid("busy info length"))
    );
}

#[test]
fn search_request_route_back() {
    let frame = Frame::SearchRequest {
        discovery_endpoint: Hpai::ROUTE_BACK,
    };
    assert_eq!(
        frame.to_vec().unwrap(),
        hex("06 10 02 01 00 0E 08 01 00 00 00 00 00 00")
    );
    assert!(Hpai::ROUTE_BACK.is_route_back());
}

#[test]
fn header_checks() {
    assert_eq!(
        Frame::decode(&hex("06 20 02 01 00 0E")),
        Err(DecodeError::Invalid("protocol version"))
    );
    assert_eq!(
        Frame::decode(&hex("05 10 02 01 00 0E")),
        Err(DecodeError::Invalid("header length"))
    );
    assert_eq!(
        Frame::decode(&hex("06 10 02 01 00 05")),
        Err(DecodeError::Invalid("total length"))
    );
    assert_eq!(frame_len(&hex("06 10 04 20")), Ok(None));
    assert_eq!(frame_len(&hex("06 10 04 20 00 15 04")), Ok(Some(0x15)));
    // Trailing octets after the frame are ignored; extra octets inside the body are not.
    let mut bytes = hex("06 10 02 0A 00 08 15 00");
    bytes.push(0xFF);
    assert!(Frame::decode(&bytes).is_ok());
    assert_eq!(
        Frame::decode(&hex("06 10 02 0A 00 09 15 00 FF")),
        Err(DecodeError::Invalid("body length"))
    );
    // Unknown services pass through.
    golden(
        &hex("06 10 09 50 00 08 AA BB"),
        &Frame::Other {
            service_type: 0x0950,
            body: vec![0xAA, 0xBB],
        },
    );
}

fn arb_hpai() -> impl Strategy<Value = Hpai> {
    (any::<bool>(), any::<[u8; 4]>(), any::<u16>()).prop_map(|(tcp, ip, port)| Hpai {
        protocol: if tcp {
            HostProtocol::Tcp
        } else {
            HostProtocol::Udp
        },
        address: SocketAddrV4::new(Ipv4Addr::from(ip), port),
    })
}

fn arb_frame() -> impl Strategy<Value = Frame> {
    let bytes = || prop::collection::vec(any::<u8>(), 0..32);
    prop_oneof![
        arb_hpai().prop_map(|discovery_endpoint| Frame::SearchRequest { discovery_endpoint }),
        (arb_hpai(), arb_hpai(), prop::option::of(any::<u16>())).prop_map(|(c, d, addr)| {
            Frame::ConnectRequest {
                control_endpoint: c,
                data_endpoint: d,
                cri: Cri::Tunnel {
                    layer: TunnelLayer::LinkLayer,
                    address: addr.map(IndividualAddress::from_raw),
                },
            }
        }),
        (any::<u8>(), arb_hpai(), any::<u16>()).prop_map(|(channel_id, d, addr)| {
            Frame::ConnectResponse {
                channel_id,
                status: Status::NO_ERROR,
                data_endpoint: Some(d),
                crd: Some(Crd::Tunnel {
                    address: IndividualAddress::from_raw(addr),
                }),
            }
        }),
        (any::<u8>(), arb_hpai()).prop_map(|(channel_id, control_endpoint)| {
            Frame::ConnectionStateRequest {
                channel_id,
                control_endpoint,
            }
        }),
        (any::<u8>(), any::<u8>()).prop_map(|(channel_id, s)| Frame::DisconnectResponse {
            channel_id,
            status: Status(s)
        }),
        (any::<u8>(), any::<u8>(), bytes()).prop_map(|(channel_id, sequence, cemi)| {
            Frame::TunnellingRequest {
                channel_id,
                sequence,
                cemi,
            }
        }),
        (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(channel_id, sequence, s)| {
            Frame::TunnellingAck {
                channel_id,
                sequence,
                status: Status(s),
            }
        }),
        bytes().prop_map(|cemi| Frame::RoutingIndication { cemi }),
        (any::<u8>(), any::<u16>(), any::<u16>()).prop_map(
            |(device_state, wait_time_ms, control)| {
                Frame::RoutingBusy {
                    device_state,
                    wait_time_ms,
                    control,
                }
            }
        ),
        (
            arb_hpai(),
            prop::collection::vec((any::<u8>(), any::<u8>()), 0..6)
        )
            .prop_map(|(c, f)| {
                Frame::SearchResponse {
                    control_endpoint: c,
                    dibs: vec![Dib::SupportedServiceFamilies(
                        f.into_iter()
                            .map(|(id, version)| ServiceFamily { id, version })
                            .collect(),
                    )],
                }
            }),
    ]
}

proptest! {
    #[test]
    fn frame_round_trip(frame in arb_frame()) {
        let bytes = frame.to_vec().unwrap();
        prop_assert_eq!(frame_len(&bytes), Ok(Some(bytes.len())));
        prop_assert_eq!(Frame::decode(&bytes), Ok(frame));
    }

    #[test]
    fn decode_arbitrary(mut bytes in prop::collection::vec(any::<u8>(), 0..96), service in 0x0200u16..0x0540) {
        // Give random bodies a valid header so the body decoders get exercised.
        if bytes.len() >= 6 {
            let len = bytes.len() as u16;
            bytes[..6].copy_from_slice(&[0x06, 0x10, (service >> 8) as u8, service as u8, (len >> 8) as u8, len as u8]);
        }
        if let Ok(frame) = Frame::decode(&bytes) {
            let again = frame.to_vec().unwrap();
            prop_assert_eq!(Frame::decode(&again), Ok(frame));
        }
    }
}
