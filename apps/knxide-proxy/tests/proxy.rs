//! The proxy between the fake gateway (upstream) and real async clients (downstream).

use std::net::SocketAddr;
use std::time::Duration;

use knxide_core::{Apdu, LData, Payload, ga};
use knxide_ip::tunnel::{Backoff, SendError};
use knxide_proxy::{Proxy, ProxyConfig};
use knxide_testkit::FakeGateway;
use knxide_tokio::{Client, ClientEvent, Error, TunnelOptions, describe};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);

fn fast() -> TunnelOptions {
    let mut options = TunnelOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        ..TunnelOptions::default()
    };
    let t = &mut options.tunnel;
    t.heartbeat_interval = Duration::from_millis(300);
    t.heartbeat_timeout = Duration::from_millis(150);
    t.connect_timeout = Duration::from_millis(500);
    t.reconnect = Some(Backoff {
        initial: Duration::from_millis(50),
        max: Duration::from_millis(200),
    });
    options
}

async fn proxy_for(gateway: &FakeGateway) -> Proxy {
    let mut config = ProxyConfig::new(gateway.address());
    config.listen = "127.0.0.1:0".parse().unwrap();
    config.discovery = false;
    config.upstream = fast();
    let proxy = Proxy::start(config).await.unwrap();
    // Wait until the upstream tunnel is open: clients are refused before that.
    timeout(WAIT, async {
        while gateway.tunnel_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("proxy connects upstream");
    proxy
}

async fn client(proxy: &Proxy) -> Client {
    let addr: SocketAddr = proxy.local_addr();
    Client::connect(addr, fast()).await.unwrap()
}

async fn next_frame(
    events: &mut tokio::sync::broadcast::Receiver<ClientEvent>,
    pred: impl Fn(&LData) -> bool,
) -> LData {
    timeout(WAIT, async {
        loop {
            if let ClientEvent::Frame(f) = events.recv().await.unwrap()
                && pred(&f)
            {
                return f;
            }
        }
    })
    .await
    .expect("frame in time")
}

#[tokio::test(flavor = "multi_thread")]
async fn clients_share_one_tunnel() {
    let gateway = FakeGateway::spawn().unwrap();
    let proxy = proxy_for(&gateway).await;
    let a = client(&proxy).await;
    let b = client(&proxy).await;
    assert_eq!(
        gateway.tunnel_count(),
        1,
        "one tunnel upstream for any number of clients"
    );
    assert_eq!(a.address().unwrap().to_string(), "15.15.241");
    assert_eq!(b.address().unwrap().to_string(), "15.15.242");
    let mut b_events = b.subscribe();
    let mut a_events = a.subscribe();

    // A writes: it reaches the bus, A's write is confirmed, B sees it from A's address.
    a.group_write(ga!(1 / 2 / 3), Payload::Small(1))
        .await
        .unwrap();
    assert_eq!(gateway.value(ga!(1 / 2 / 3)), Some(Payload::Small(1)));
    let seen = next_frame(&mut b_events, |f| f.group_address() == Some(ga!(1 / 2 / 3))).await;
    assert_eq!(seen.source.to_string(), "15.15.241");
    assert_eq!(seen.apdu(), Some(&Apdu::GroupValueWrite(Payload::Small(1))));

    // Bus traffic reaches everyone.
    let mut ldata = LData::group(
        ga!(0 / 0 / 1),
        Apdu::GroupValueWrite(Payload::Bytes(vec![0x0c, 0x1a])),
    );
    ldata.source = "1.1.5".parse().unwrap();
    gateway.indicate(&ldata);
    assert_eq!(
        next_frame(&mut a_events, |f| f.source.to_string() == "1.1.5").await,
        ldata
    );
    assert_eq!(
        next_frame(&mut b_events, |f| f.source.to_string() == "1.1.5").await,
        ldata
    );

    // Reads go through and the device's answer comes back.
    gateway.set_value(ga!(1 / 2 / 4), Payload::Bytes(vec![0x0c, 0x33]));
    assert_eq!(
        b.group_read(ga!(1 / 2 / 4), WAIT).await.unwrap(),
        Payload::Bytes(vec![0x0c, 0x33])
    );

    // Many writes from both clients at once all make it.
    let mut tasks = Vec::new();
    for (i, c) in [a.clone(), b.clone()]
        .into_iter()
        .cycle()
        .take(20)
        .enumerate()
    {
        tasks.push(tokio::spawn(async move {
            c.group_write(ga!(2 / 0 / 0), Payload::Bytes(vec![i as u8]))
                .await
        }));
    }
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    // The bus side carries the proxy's upstream tunnel address only.
    let sources: std::collections::HashSet<String> = gateway
        .received()
        .iter()
        .map(|c| c.ldata().unwrap().source.to_string())
        .collect();
    assert_eq!(sources.len(), 1, "{sources:?}");
    proxy.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn negative_confirmation_reaches_the_client() {
    let gateway = FakeGateway::spawn().unwrap();
    let proxy = proxy_for(&gateway).await;
    let a = client(&proxy).await;
    gateway.behaviour(|b| b.confirm_errors = true);
    let result = a.group_write(ga!(1 / 2 / 3), Payload::Small(1)).await;
    assert!(
        matches!(result, Err(Error::Send(SendError::NegativeConfirmation))),
        "{result:?}"
    );
    proxy.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gateway_outage_and_recovery() {
    let gateway = FakeGateway::spawn().unwrap();
    let address = gateway.address();
    let proxy = proxy_for(&gateway).await;
    let a = client(&proxy).await;
    let mut events = a.subscribe();

    gateway.stop();
    // The client learns the bus is gone (its heartbeats are refused) and keeps retrying.
    timeout(WAIT, async {
        loop {
            if let ClientEvent::Disconnected { .. } = events.recv().await.unwrap() {
                return;
            }
        }
    })
    .await
    .expect("client told about the outage");

    let gateway = FakeGateway::bind(address).unwrap();
    timeout(Duration::from_secs(10), async {
        loop {
            if let ClientEvent::Connected { .. } = events.recv().await.unwrap() {
                return;
            }
        }
    })
    .await
    .expect("client back after the gateway returns");
    a.group_write(ga!(1 / 2 / 3), Payload::Small(0))
        .await
        .unwrap();
    assert_eq!(gateway.value(ga!(1 / 2 / 3)), Some(Payload::Small(0)));
    proxy.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn discoverable_and_closes_cleanly() {
    let gateway = FakeGateway::spawn().unwrap();
    let proxy = proxy_for(&gateway).await;
    let found = describe(proxy.local_addr(), WAIT).await.unwrap().unwrap();
    assert_eq!(found.name(), "knxide proxy");
    assert!(found.supports_tunnelling());

    let a = client(&proxy).await;
    let mut events = a.subscribe();
    proxy.stop().await;
    timeout(WAIT, async {
        loop {
            if let ClientEvent::Disconnected { .. } = events.recv().await.unwrap() {
                return;
            }
        }
    })
    .await
    .expect("client told the proxy went away");
    assert_eq!(gateway.tunnel_count(), 0, "upstream tunnel closed");
}
