//! Tunnelling over TCP against the fake gateway's TCP mode.

use std::time::Duration;

use knxide_core::dpt::{DptId, DptValue};
use knxide_core::{Apdu, LData, Payload, ga};
use knxide_ip::tunnel::{Backoff, DisconnectReason, Transport};
use knxide_testkit::FakeGateway;
use knxide_tokio::{Client, ClientEvent, Error, TunnelOptions};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);

fn tcp_options() -> TunnelOptions {
    let mut options = TunnelOptions::default();
    let t = &mut options.tunnel;
    t.transport = Transport::Tcp;
    t.heartbeat_interval = Duration::from_millis(200);
    t.heartbeat_timeout = Duration::from_millis(100);
    t.connect_timeout = Duration::from_millis(300);
    t.reconnect = Some(Backoff {
        initial: Duration::from_millis(50),
        max: Duration::from_millis(200),
    });
    options
}

async fn next_event(
    events: &mut tokio::sync::broadcast::Receiver<ClientEvent>,
    mut pred: impl FnMut(&ClientEvent) -> bool,
) -> ClientEvent {
    timeout(WAIT, async {
        loop {
            let event = events.recv().await.unwrap();
            if pred(&event) {
                return event;
            }
        }
    })
    .await
    .expect("event in time")
}

#[tokio::test]
async fn write_read_and_receive_over_tcp() {
    let gateway = FakeGateway::spawn_tcp().unwrap();
    let client = Client::connect(gateway.address(), tcp_options())
        .await
        .unwrap();
    let mut events = client.subscribe();

    let dpt = DptId::new(9, 1);
    client
        .group_write_value(ga!(1 / 2 / 4), dpt, &DptValue::Float(19.5))
        .await
        .unwrap();
    assert_eq!(
        client
            .group_read_value(ga!(1 / 2 / 4), dpt, WAIT)
            .await
            .unwrap(),
        DptValue::Float(19.5)
    );

    let mut ldata = LData::group(ga!(0 / 0 / 1), Apdu::GroupValueWrite(Payload::Small(1)));
    ldata.source = "1.1.5".parse().unwrap();
    gateway.indicate(&ldata);
    assert_eq!(
        next_event(
            &mut events,
            |e| matches!(e, ClientEvent::Frame(f) if f.source.to_string() == "1.1.5")
        )
        .await,
        ClientEvent::Frame(ldata)
    );

    // Heartbeats keep it open; no tunnelling acks are exchanged.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(client.address().is_some());
    assert!(
        !gateway
            .frames()
            .iter()
            .any(|f| matches!(f, knxide_ip::Frame::TunnellingAck { .. }))
    );
}

#[tokio::test]
async fn reconnects_when_the_stream_drops() {
    let gateway = FakeGateway::spawn_tcp().unwrap();
    let client = Client::connect(gateway.address(), tcp_options())
        .await
        .unwrap();
    let mut events = client.subscribe();
    gateway.drop_connections();
    let event = next_event(&mut events, |e| {
        matches!(e, ClientEvent::Disconnected { .. })
    })
    .await;
    assert_eq!(
        event,
        ClientEvent::Disconnected {
            reason: DisconnectReason::TransportClosed
        }
    );
    next_event(&mut events, |e| matches!(e, ClientEvent::Connected { .. })).await;
    client
        .group_write(ga!(1 / 2 / 3), Payload::Small(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn reconnects_after_gateway_restart() {
    let gateway = FakeGateway::spawn_tcp().unwrap();
    let address = gateway.address();
    let client = Client::connect(address, tcp_options()).await.unwrap();
    let mut events = client.subscribe();
    gateway.stop();
    next_event(&mut events, |e| {
        matches!(e, ClientEvent::Disconnected { .. })
    })
    .await;
    let restarted = FakeGateway::bind_tcp(address).unwrap();
    next_event(&mut events, |e| matches!(e, ClientEvent::Connected { .. })).await;
    client
        .group_write(ga!(1 / 2 / 3), Payload::Small(1))
        .await
        .unwrap();
    assert_eq!(restarted.received().len(), 1);
}

#[tokio::test]
async fn server_disconnect_over_tcp() {
    let gateway = FakeGateway::spawn_tcp().unwrap();
    let client = Client::connect(gateway.address(), tcp_options())
        .await
        .unwrap();
    let mut events = client.subscribe();
    gateway.disconnect_all();
    let event = next_event(&mut events, |e| {
        matches!(e, ClientEvent::Disconnected { .. })
    })
    .await;
    assert_eq!(
        event,
        ClientEvent::Disconnected {
            reason: DisconnectReason::ServerDisconnected
        }
    );
    next_event(&mut events, |e| matches!(e, ClientEvent::Connected { .. })).await;
}

#[tokio::test]
async fn nothing_listening() {
    let unused = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let result = Client::connect(unused, tcp_options()).await;
    assert!(
        matches!(
            result,
            Err(Error::Connect(DisconnectReason::TransportClosed))
        ),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
async fn close_over_tcp() {
    let gateway = FakeGateway::spawn_tcp().unwrap();
    let client = Client::connect(gateway.address(), tcp_options())
        .await
        .unwrap();
    timeout(WAIT, client.close()).await.unwrap();
    assert_eq!(gateway.tunnel_count(), 0);
}
