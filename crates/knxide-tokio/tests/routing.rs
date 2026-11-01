//! The routing client against a scripted router socket.
//!
//! The router is reached by unicast on loopback so the tests do not depend on multicast
//! being available; the protocol is the same.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use knxide_core::{Apdu, Cemi, LData, Payload, ga};
use knxide_ip::Frame;
use knxide_ip::router::RouterConfig;
use knxide_tokio::{Client, ClientEvent, ConnectionState, RoutingOptions};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);

struct FakeRouter {
    socket: UdpSocket,
}

impl FakeRouter {
    fn new() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        Self { socket }
    }

    fn address(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    /// The next routing indication and who sent it.
    fn receive(&self) -> (Cemi, SocketAddr) {
        let mut buf = [0u8; 512];
        let (len, source) = self.socket.recv_from(&mut buf).unwrap();
        let Ok(Frame::RoutingIndication { cemi }) = Frame::decode(&buf[..len]) else {
            panic!()
        };
        (Cemi::decode(&cemi).unwrap(), source)
    }

    fn send(&self, frame: &Frame, to: SocketAddr) {
        self.socket.send_to(&frame.to_vec().unwrap(), to).unwrap();
    }
}

async fn client(router: &FakeRouter) -> Client {
    let options = RoutingOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        router: RouterConfig {
            group: router.address(),
            ..RouterConfig::default()
        },
        ..RoutingOptions::default()
    };
    Client::routing(options).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn write_and_receive() {
    let router = FakeRouter::new();
    let client = client(&router).await;
    assert_eq!(
        client.state(),
        ConnectionState::Connected("15.15.250".parse().unwrap())
    );
    let mut events = client.subscribe();

    client
        .group_write(ga!(1 / 2 / 3), Payload::Small(1))
        .await
        .unwrap();
    let (cemi, client_addr) = tokio::task::block_in_place(|| router.receive());
    let Cemi::LDataInd(ldata) = cemi else {
        panic!("{cemi:?}")
    };
    assert_eq!(ldata.source.to_string(), "15.15.250");
    assert_eq!(
        ldata.apdu(),
        Some(&Apdu::GroupValueWrite(Payload::Small(1)))
    );

    let mut indication = LData::group(
        ga!(0 / 0 / 1),
        Apdu::GroupValueWrite(Payload::Bytes(vec![0x0C, 0x1A])),
    );
    indication.source = "1.1.5".parse().unwrap();
    router.send(
        &Frame::RoutingIndication {
            cemi: Cemi::LDataInd(indication.clone()).to_vec().unwrap(),
        },
        client_addr,
    );
    let event = timeout(WAIT, async {
        loop {
            if let ClientEvent::Frame(f) = events.recv().await.unwrap() {
                return f;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(event, indication);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_over_routing() {
    let router = FakeRouter::new();
    let client = client(&router).await;
    let reader = {
        let client = client.clone();
        tokio::spawn(async move { client.group_read(ga!(1 / 2 / 4), WAIT).await })
    };
    let (cemi, client_addr) = tokio::task::block_in_place(|| router.receive());
    assert_eq!(cemi.ldata().unwrap().apdu(), Some(&Apdu::GroupValueRead));
    let mut response = LData::group(
        ga!(1 / 2 / 4),
        Apdu::GroupValueResponse(Payload::Bytes(vec![0x0C, 0x1A])),
    );
    response.source = "1.1.10".parse().unwrap();
    router.send(
        &Frame::RoutingIndication {
            cemi: Cemi::LDataInd(response).to_vec().unwrap(),
        },
        client_addr,
    );
    assert_eq!(
        reader.await.unwrap().unwrap(),
        Payload::Bytes(vec![0x0C, 0x1A])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_delays_sending() {
    let router = FakeRouter::new();
    let client = client(&router).await;
    client
        .group_write(ga!(1 / 2 / 3), Payload::Small(1))
        .await
        .unwrap();
    let (_, client_addr) = tokio::task::block_in_place(|| router.receive());

    router.send(
        &Frame::RoutingBusy {
            device_state: 0,
            wait_time_ms: 300,
            control: 0,
        },
        client_addr,
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    let start = Instant::now();
    client
        .group_write(ga!(1 / 2 / 3), Payload::Small(0))
        .await
        .unwrap();
    assert!(
        start.elapsed() >= Duration::from_millis(200),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limited_to_about_fifty_per_second() {
    let router = FakeRouter::new();
    let client = client(&router).await;
    let start = Instant::now();
    for i in 0..11u8 {
        client
            .group_write(ga!(1 / 2 / 3), Payload::Bytes(vec![i]))
            .await
            .unwrap();
    }
    // Ten gaps of at least 20 ms.
    assert!(
        start.elapsed() >= Duration::from_millis(200),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn close() {
    let router = FakeRouter::new();
    let client = client(&router).await;
    let other = client.clone();
    timeout(WAIT, client.close()).await.unwrap();
    assert_eq!(other.state(), ConnectionState::Closed);
}

/// Real multicast. Needs a multicast-capable interface and a firewall that lets KNXnet/IP
/// multicast in, so not run by default. Set `KNXIDE_TEST_INTERFACE` to pick the interface:
/// `KNXIDE_TEST_INTERFACE=127.0.0.1 cargo test -p knxide-tokio --test routing -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn multicast_between_two_clients() {
    let interface = std::env::var("KNXIDE_TEST_INTERFACE")
        .ok()
        .map(|i| i.parse().unwrap());
    let options = |address: &str| RoutingOptions {
        interface,
        multicast_loop: true,
        router: RouterConfig {
            individual_address: address.parse().unwrap(),
            ..RouterConfig::default()
        },
        ..RoutingOptions::default()
    };
    let a = Client::routing(options("15.15.201")).await.unwrap();
    let b = Client::routing(options("15.15.202")).await.unwrap();
    let mut events = b.subscribe();
    a.group_write(ga!(31 / 7 / 255), Payload::Small(1))
        .await
        .unwrap();
    let frame = timeout(WAIT, async {
        loop {
            if let ClientEvent::Frame(f) = events.recv().await.unwrap()
                && f.source.to_string() == "15.15.201"
            {
                return f;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(frame.group_address(), Some(ga!(31 / 7 / 255)));
}
