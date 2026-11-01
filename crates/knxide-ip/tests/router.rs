//! Routing client tests with a fake clock.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use knxide_core::{Apdu, Cemi, LData, Payload, ga};
use knxide_ip::Frame;
use knxide_ip::discovery::MULTICAST_ENDPOINT;
use knxide_ip::router::{RouterClient, RouterConfig, RouterEvent};

const MS: Duration = Duration::from_millis(1);
const ROUTER: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 60), 3671));

fn write(value: u8) -> Cemi {
    Cemi::LDataReq(LData::group(
        ga!(1 / 2 / 3),
        Apdu::GroupValueWrite(Payload::Small(value)),
    ))
}

fn sent(router: &mut RouterClient) -> Vec<Cemi> {
    std::iter::from_fn(|| router.poll_transmit())
        .map(|tx| {
            assert_eq!(tx.destination, MULTICAST_ENDPOINT);
            let Ok(Frame::RoutingIndication { cemi }) = Frame::decode(&tx.payload) else {
                panic!()
            };
            Cemi::decode(&cemi).unwrap()
        })
        .collect()
}

fn busy(ms: u16) -> Vec<u8> {
    Frame::RoutingBusy {
        device_state: 0,
        wait_time_ms: ms,
        control: 0,
    }
    .to_vec()
    .unwrap()
}

#[test]
fn sends_ldata_ind_with_configured_source() {
    let now = Instant::now();
    let mut router = RouterClient::new(RouterConfig::default());
    let id = router.send(&write(1), now).unwrap();
    let frames = sent(&mut router);
    let [Cemi::LDataInd(ldata)] = frames.as_slice() else {
        panic!("{frames:?}")
    };
    assert_eq!(ldata.source.to_string(), "15.15.250");
    assert_eq!(ldata.group_address(), Some(ga!(1 / 2 / 3)));
    assert_eq!(router.poll_event(), Some(RouterEvent::Sent(id)));
    assert_eq!(router.poll_timeout(), None);
}

#[test]
fn rate_limit() {
    let now = Instant::now();
    let mut router = RouterClient::new(RouterConfig::default());
    for v in 0..3 {
        router.send(&write(v), now).unwrap();
    }
    assert_eq!(sent(&mut router).len(), 1);
    assert_eq!(router.poll_timeout(), Some(now + 20 * MS));
    router.handle_timeout(now + 19 * MS);
    assert!(sent(&mut router).is_empty());
    router.handle_timeout(now + 20 * MS);
    assert_eq!(sent(&mut router).len(), 1);
    router.handle_timeout(now + 40 * MS);
    assert_eq!(sent(&mut router).len(), 1);
    assert_eq!(router.poll_timeout(), None);
}

#[test]
fn busy_pauses_sending() {
    let now = Instant::now();
    let mut router = RouterClient::new(RouterConfig::default());
    router.handle_input(ROUTER, &busy(100), now);
    assert_eq!(
        router.poll_event(),
        Some(RouterEvent::Busy { wait: 100 * MS })
    );
    assert!(router.is_paused(now));
    router.send(&write(1), now + MS).unwrap();
    assert!(sent(&mut router).is_empty());
    // First busy frame: no random extension.
    assert_eq!(router.poll_timeout(), Some(now + 100 * MS));
    router.handle_timeout(now + 100 * MS);
    assert_eq!(sent(&mut router).len(), 1);
}

#[test]
fn repeated_busy_extends_pause() {
    let now = Instant::now();
    let mut router = RouterClient::new(RouterConfig::default());
    router.handle_input(ROUTER, &busy(100), now);
    // A second busy frame more than 10 ms later counts, and adds up to 50 ms at random.
    router.handle_input(ROUTER, &busy(100), now + 20 * MS);
    router.send(&write(1), now + 21 * MS).unwrap();
    let resume = router.poll_timeout().unwrap();
    assert!(
        resume >= now + 120 * MS && resume <= now + 170 * MS,
        "{:?}",
        resume - now
    );
    // A shorter wait than what remains is ignored.
    router.handle_input(ROUTER, &busy(10), now + 40 * MS);
    assert_eq!(router.poll_timeout(), Some(resume));
}

#[test]
fn busy_count_decays() {
    let now = Instant::now();
    let mut router = RouterClient::new(RouterConfig::default());
    router.handle_input(ROUTER, &busy(50), now);
    router.handle_input(ROUTER, &busy(50), now + 20 * MS);
    // Long after the pause and slow-down, a new busy frame starts from zero again.
    let later = now + Duration::from_secs(5);
    router.handle_input(ROUTER, &busy(50), later);
    router.send(&write(1), later).unwrap();
    assert_eq!(router.poll_timeout(), Some(later + 50 * MS));
}

#[test]
fn receives_indications_and_ignores_own_frames() {
    let now = Instant::now();
    let own: SocketAddr = "192.168.1.10:3671".parse().unwrap();
    let mut router = RouterClient::new(RouterConfig {
        own_endpoint: Some(own),
        ..RouterConfig::default()
    });
    let mut ldata = LData::group(ga!(0 / 0 / 1), Apdu::GroupValueWrite(Payload::Small(1)));
    ldata.source = "1.1.5".parse().unwrap();
    let datagram = Frame::RoutingIndication {
        cemi: Cemi::LDataInd(ldata.clone()).to_vec().unwrap(),
    }
    .to_vec()
    .unwrap();
    router.handle_input(own, &datagram, now);
    assert_eq!(router.poll_event(), None);
    router.handle_input(ROUTER, &datagram, now);
    assert_eq!(
        router.poll_event(),
        Some(RouterEvent::Received(Cemi::LDataInd(ldata)))
    );
}

#[test]
fn lost_messages() {
    let now = Instant::now();
    let mut router = RouterClient::new(RouterConfig::default());
    let datagram = Frame::RoutingLostMessage {
        device_state: 1,
        lost_messages: 7,
    }
    .to_vec()
    .unwrap();
    router.handle_input(ROUTER, &datagram, now);
    assert_eq!(
        router.poll_event(),
        Some(RouterEvent::LostMessages {
            count: 7,
            device_state: 1
        })
    );
}

#[test]
fn only_ldata_can_be_routed() {
    let mut router = RouterClient::new(RouterConfig::default());
    let other = Cemi::Other {
        message_code: 0xFC,
        data: vec![],
    };
    assert!(router.send(&other, Instant::now()).is_err());
}
