//! Discovery state machine tests.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use knxide_core::IndividualAddress;
use knxide_ip::discovery::{Discovery, MULTICAST_ENDPOINT};
use knxide_ip::structures::Medium;
use knxide_ip::{DeviceInfo, Dib, Frame, Hpai, ServiceFamily};

fn device(name: &str) -> Dib {
    Dib::DeviceInfo(DeviceInfo {
        medium: Medium::TP1,
        status: 0,
        address: IndividualAddress::from_raw(0x1100),
        project_installation_id: 0,
        serial_number: [0; 6],
        multicast_address: Ipv4Addr::new(224, 0, 23, 12),
        mac_address: [0; 6],
        name: name.to_string(),
    })
}

fn families(ids: &[u8]) -> Dib {
    Dib::SupportedServiceFamilies(
        ids.iter()
            .map(|&id| ServiceFamily { id, version: 1 })
            .collect(),
    )
}

fn addr(last: u8, port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, last), port))
}

#[test]
fn search_collects_unique_gateways_until_timeout() {
    let now = Instant::now();
    let local = Hpai::udp(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 50000));
    let mut d = Discovery::search(local, Duration::from_secs(3), now);
    let tx = d.poll_transmit().unwrap();
    assert_eq!(tx.destination, MULTICAST_ENDPOINT);
    assert_eq!(
        Frame::decode(&tx.payload),
        Ok(Frame::SearchRequest {
            discovery_endpoint: local
        })
    );
    assert!(d.poll_transmit().is_none());

    let response = |name: &str, ids: &[u8], endpoint: Hpai| {
        Frame::SearchResponse {
            control_endpoint: endpoint,
            dibs: vec![device(name), families(ids)],
        }
        .to_vec()
        .unwrap()
    };
    let interface = Hpai::udp(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 50), 3671));
    d.handle_input(
        addr(50, 3671),
        &response("Interface", &[2, 4], interface),
        now,
    );
    d.handle_input(
        addr(50, 3671),
        &response("Interface", &[2, 4], interface),
        now,
    );
    // A server answering with route-back is reached at the source address.
    d.handle_input(
        addr(51, 40000),
        &response("Router", &[2, 4, 5, 9], Hpai::ROUTE_BACK),
        now,
    );
    d.handle_input(addr(52, 3671), &[0x06, 0x10, 0x02], now);

    let first = d.poll_gateway().unwrap();
    assert_eq!(first.name(), "Interface");
    assert_eq!(first.control_endpoint, addr(50, 3671));
    assert!(first.supports_tunnelling());
    assert!(!first.supports_routing());
    let second = d.poll_gateway().unwrap();
    assert_eq!(second.control_endpoint, addr(51, 40000));
    assert!(second.supports_routing() && second.supports_security());
    assert!(d.poll_gateway().is_none());
    assert_eq!(d.gateways().len(), 2);

    let later = now + Duration::from_secs(3);
    assert_eq!(d.poll_timeout(), later);
    assert!(d.is_done(later));
    d.handle_input(
        addr(53, 3671),
        &response("Late", &[2, 4], Hpai::ROUTE_BACK),
        later,
    );
    assert_eq!(d.gateways().len(), 2);
}

#[test]
fn describe_one_host() {
    let now = Instant::now();
    let target = addr(50, 3671);
    let mut d = Discovery::describe(target, Hpai::ROUTE_BACK, Duration::from_secs(2), now);
    let tx = d.poll_transmit().unwrap();
    assert_eq!(tx.destination, target);
    assert_eq!(
        Frame::decode(&tx.payload),
        Ok(Frame::DescriptionRequest {
            control_endpoint: Hpai::ROUTE_BACK
        })
    );
    let response = Frame::DescriptionResponse {
        dibs: vec![device("Interface"), families(&[2, 4])],
    };
    d.handle_input(target, &response.to_vec().unwrap(), now);
    let gw = d.poll_gateway().unwrap();
    assert_eq!(gw.control_endpoint, target);
    assert_eq!(gw.name(), "Interface");
}
