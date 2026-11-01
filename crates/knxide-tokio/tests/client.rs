//! The async client against the fake gateway.

use std::time::Duration;

use knxide_core::dpt::{self, DptId, DptValue};
use knxide_core::{Apdu, LData, Payload, ga};
use knxide_ip::Status;
use knxide_ip::tunnel::{Backoff, DisconnectReason, SendError};
use knxide_testkit::FakeGateway;
use knxide_tokio::{Client, ClientEvent, ConnectionState, Error, TunnelOptions, describe};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);

fn fast_options() -> TunnelOptions {
    let mut options = TunnelOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        ..TunnelOptions::default()
    };
    let t = &mut options.tunnel;
    t.heartbeat_interval = Duration::from_millis(200);
    t.heartbeat_timeout = Duration::from_millis(100);
    t.ack_timeout = Duration::from_millis(100);
    t.connect_timeout = Duration::from_millis(300);
    t.reconnect = Some(Backoff {
        initial: Duration::from_millis(50),
        max: Duration::from_millis(200),
    });
    options
}

async fn connect(gateway: &FakeGateway) -> Client {
    Client::connect(gateway.address(), fast_options())
        .await
        .unwrap()
}

/// Wait for the first event matching `pred`.
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
async fn connect_write_read() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    assert!(matches!(client.state(), ConnectionState::Connected(_)));
    assert!(client.address().is_some());

    client
        .group_write(ga!(1 / 2 / 3), dpt::dpt1::encode(true))
        .await
        .unwrap();
    assert_eq!(gateway.value(ga!(1 / 2 / 3)), Some(Payload::Small(1)));

    let temperature = DptId::new(9, 1);
    client
        .group_write_value(ga!(1 / 2 / 4), temperature, &DptValue::Float(21.5))
        .await
        .unwrap();
    let value = client
        .group_read_value(ga!(1 / 2 / 4), temperature, WAIT)
        .await
        .unwrap();
    assert_eq!(value, DptValue::Float(21.5));
}

#[tokio::test]
async fn read_times_out_without_a_responder() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    let result = client
        .group_read(ga!(5 / 5 / 5), Duration::from_millis(200))
        .await;
    assert!(matches!(result, Err(Error::Timeout)), "{result:?}");
}

#[tokio::test]
async fn subscribers_see_bus_traffic() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    let mut events = client.subscribe();
    let mut ldata = LData::group(
        ga!(0 / 0 / 1),
        Apdu::GroupValueWrite(Payload::Bytes(vec![0x0C, 0x1A])),
    );
    ldata.source = "1.1.5".parse().unwrap();
    gateway.indicate(&ldata);
    let event = next_event(&mut events, |e| matches!(e, ClientEvent::Frame(_))).await;
    assert_eq!(event, ClientEvent::Frame(ldata));
}

#[tokio::test]
async fn concurrent_writes_from_clones() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    let mut tasks = Vec::new();
    for i in 0..20u8 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            client
                .group_write(ga!(2 / 0 / 0), Payload::Bytes(vec![i]))
                .await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(gateway.received().len(), 20);
}

#[tokio::test]
async fn local_echo() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut options = fast_options();
    options.local_echo = true;
    let client = Client::connect(gateway.address(), options).await.unwrap();
    let mut events = client.subscribe();
    client
        .group_write(ga!(1 / 1 / 1), Payload::Small(1))
        .await
        .unwrap();
    let ClientEvent::Frame(ldata) =
        next_event(&mut events, |e| matches!(e, ClientEvent::Frame(_))).await
    else {
        unreachable!()
    };
    assert_eq!(ldata.group_address(), Some(ga!(1 / 1 / 1)));
    assert_eq!(Some(ldata.source), client.address());
}

#[tokio::test]
async fn negative_confirmation_is_an_error() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    gateway.behaviour(|b| b.confirm_errors = true);
    let result = client.group_write(ga!(1 / 2 / 3), Payload::Small(1)).await;
    assert!(
        matches!(result, Err(Error::Send(SendError::NegativeConfirmation))),
        "{result:?}"
    );
}

#[tokio::test]
async fn first_connect_refused() {
    let gateway = FakeGateway::spawn().unwrap();
    gateway.behaviour(|b| b.refuse_connections = Some(Status::NO_MORE_CONNECTIONS));
    let result = Client::connect(gateway.address(), fast_options()).await;
    assert!(
        matches!(
            result,
            Err(Error::Connect(DisconnectReason::ConnectRejected(
                Status::NO_MORE_CONNECTIONS
            )))
        ),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
async fn first_connect_times_out() {
    // Bound but never answering.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let result = Client::connect(silent.local_addr().unwrap(), fast_options()).await;
    assert!(
        matches!(
            result,
            Err(Error::Connect(DisconnectReason::ConnectTimeout))
        ),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
async fn reconnects_after_gateway_restart() {
    let gateway = FakeGateway::spawn().unwrap();
    let address = gateway.address();
    let client = connect(&gateway).await;
    let mut events = client.subscribe();

    gateway.stop();
    let restarted = FakeGateway::bind(address).unwrap();
    let event = next_event(&mut events, |e| {
        matches!(e, ClientEvent::Disconnected { .. })
    })
    .await;
    assert!(matches!(
        event,
        ClientEvent::Disconnected {
            reason: DisconnectReason::HeartbeatFailed
        }
    ));
    next_event(&mut events, |e| matches!(e, ClientEvent::Connected { .. })).await;

    client
        .group_write(ga!(1 / 2 / 3), Payload::Small(0))
        .await
        .unwrap();
    assert_eq!(restarted.received().len(), 1);
}

#[tokio::test]
async fn writes_during_an_outage_are_sent_after_reconnect() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    let mut events = client.subscribe();
    gateway.behaviour(|b| b.refuse_connections = Some(Status::NO_MORE_CONNECTIONS));
    gateway.disconnect_all();
    next_event(&mut events, |e| {
        matches!(e, ClientEvent::Disconnected { .. })
    })
    .await;
    assert_eq!(client.state(), ConnectionState::Connecting);

    let write = {
        let client = client.clone();
        tokio::spawn(async move { client.group_write(ga!(3 / 3 / 3), Payload::Small(1)).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    gateway.behaviour(|b| b.refuse_connections = None);
    timeout(WAIT, write).await.unwrap().unwrap().unwrap();
    assert_eq!(gateway.value(ga!(3 / 3 / 3)), Some(Payload::Small(1)));
}

#[tokio::test]
async fn close() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    let other = client.clone();
    timeout(WAIT, client.close()).await.unwrap();
    assert_eq!(other.state(), ConnectionState::Closed);
    assert_eq!(gateway.tunnel_count(), 0);
    assert!(matches!(
        other.group_write(ga!(1 / 2 / 3), Payload::Small(1)).await,
        Err(Error::Closed)
    ));
}

#[tokio::test]
async fn dropping_every_handle_disconnects() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    drop(client);
    timeout(WAIT, async {
        while gateway.tunnel_count() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("tunnel closed");
}

#[tokio::test]
async fn non_nat_mode_advertises_local_address() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut options = fast_options();
    options.nat = false;
    let client = Client::connect(gateway.address(), options).await.unwrap();
    client
        .group_write(ga!(1 / 2 / 3), Payload::Small(1))
        .await
        .unwrap();
    let advertised = gateway.frames().into_iter().find_map(|f| match f {
        knxide_ip::Frame::ConnectRequest {
            control_endpoint, ..
        } => Some(control_endpoint),
        _ => None,
    });
    assert!(!advertised.unwrap().is_route_back());
}

#[tokio::test]
async fn describe_gateway() {
    let gateway = FakeGateway::spawn().unwrap();
    let found = describe(gateway.address(), WAIT).await.unwrap().unwrap();
    assert_eq!(found.name(), "knxide fake gateway");
    assert!(found.supports_tunnelling());
    assert_eq!(found.control_endpoint, gateway.address());
}

#[tokio::test]
async fn restart_device() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    let device = "1.1.10".parse().unwrap();
    client.restart_device(device).await.unwrap();
    assert_eq!(gateway.restarts(), [device]);
    // Connect, restart on the connection, disconnect: all at system priority.
    let tpdus: Vec<_> = gateway
        .received()
        .iter()
        .map(|c| c.ldata().unwrap().tpdu.clone())
        .collect();
    assert_eq!(
        tpdus,
        [
            knxide_core::Tpdu::Connect,
            knxide_core::Tpdu::Connected {
                sequence: 0,
                apdu: Apdu::restart()
            },
            knxide_core::Tpdu::Disconnect,
        ]
    );
    assert!(
        gateway
            .received()
            .iter()
            .all(|c| c.ldata().unwrap().priority == knxide_core::Priority::System)
    );
}

#[tokio::test]
async fn restart_unknown_device() {
    let gateway = FakeGateway::spawn().unwrap();
    let client = connect(&gateway).await;
    let nobody = "1.1.99".parse().unwrap();
    let result = client.restart_device(nobody).await;
    assert!(
        matches!(result, Err(Error::DeviceNotResponding(a)) if a == nobody),
        "{result:?}"
    );
    assert!(gateway.restarts().is_empty());
}
