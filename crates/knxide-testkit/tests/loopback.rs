//! The sans-IO tunnel client against the fake gateway over loopback UDP.

use std::time::Duration;

use knxide_core::{Apdu, Cemi, LData, Payload, ga};
use knxide_ip::Status;
use knxide_ip::tunnel::{DisconnectReason, Event, SendError, TunnelConfig};
use knxide_testkit::{BlockingTunnel, FakeGateway};

const WAIT: Duration = Duration::from_secs(5);

fn fast_config(gateway: &FakeGateway) -> TunnelConfig {
    let mut config = TunnelConfig::new(gateway.address());
    config.heartbeat_interval = Duration::from_millis(200);
    config.heartbeat_timeout = Duration::from_millis(100);
    config.ack_timeout = Duration::from_millis(100);
    config.connect_timeout = Duration::from_millis(300);
    config.reconnect = Some(knxide_ip::tunnel::Backoff {
        initial: Duration::from_millis(50),
        max: Duration::from_millis(200),
    });
    config
}

fn connected(gateway: &FakeGateway) -> BlockingTunnel {
    let mut tunnel = BlockingTunnel::connect(fast_config(gateway)).unwrap();
    tunnel
        .wait_for(|e| matches!(e, Event::Connected { .. }), WAIT)
        .expect("connects");
    tunnel
}

fn write(value: u8) -> Cemi {
    Cemi::LDataReq(LData::group(
        ga!(1 / 2 / 3),
        Apdu::GroupValueWrite(Payload::Small(value)),
    ))
}

#[test]
fn write_is_confirmed_and_reaches_the_bus() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut tunnel = connected(&gateway);
    let id = tunnel.send(&write(1)).unwrap();
    assert_eq!(tunnel.wait_for_send(id, WAIT), Some(Ok(())));
    let received = gateway.received();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0].ldata().unwrap().source,
        tunnel.client.address().unwrap()
    );
    assert_eq!(gateway.value(ga!(1 / 2 / 3)), Some(Payload::Small(1)));
}

#[test]
fn read_gets_a_response() {
    let gateway = FakeGateway::spawn().unwrap();
    gateway.set_value(ga!(1 / 2 / 4), Payload::Bytes(vec![0x0C, 0x1A]));
    let mut tunnel = connected(&gateway);
    let read = Cemi::LDataReq(LData::group(ga!(1 / 2 / 4), Apdu::GroupValueRead));
    tunnel.send(&read).unwrap();
    let response = tunnel
        .wait_for(|e| matches!(e, Event::Received(_)), WAIT)
        .unwrap();
    let Event::Received(Cemi::LDataInd(ldata)) = response else {
        panic!("{response:?}")
    };
    assert_eq!(ldata.group_address(), Some(ga!(1 / 2 / 4)));
    assert_eq!(
        ldata.apdu(),
        Some(&Apdu::GroupValueResponse(Payload::Bytes(vec![0x0C, 0x1A])))
    );
}

#[test]
fn bus_traffic_is_received() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut tunnel = connected(&gateway);
    let mut ldata = LData::group(ga!(0 / 0 / 1), Apdu::GroupValueWrite(Payload::Small(1)));
    ldata.source = "1.1.5".parse().unwrap();
    for _ in 0..3 {
        gateway.indicate(&ldata);
    }
    for _ in 0..3 {
        let event = tunnel.wait_for(|e| matches!(e, Event::Received(_)), WAIT);
        assert_eq!(event, Some(Event::Received(Cemi::LDataInd(ldata.clone()))));
    }
}

#[test]
fn heartbeats_keep_the_tunnel_open() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut tunnel = connected(&gateway);
    let lost = tunnel.wait_for(
        |e| matches!(e, Event::Disconnected { .. }),
        Duration::from_millis(800),
    );
    assert_eq!(lost, None);
    let heartbeats = gateway
        .frames()
        .iter()
        .filter(|f| matches!(f, knxide_ip::Frame::ConnectionStateRequest { .. }))
        .count();
    assert!(heartbeats >= 3, "{heartbeats} heartbeats");
}

#[test]
fn ignored_heartbeats_cause_reconnect() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut tunnel = connected(&gateway);
    gateway.behaviour(|b| b.ignore_heartbeats = true);
    let event = tunnel
        .wait_for(|e| matches!(e, Event::Disconnected { .. }), WAIT)
        .unwrap();
    assert!(matches!(
        event,
        Event::Disconnected {
            reason: DisconnectReason::HeartbeatFailed,
            ..
        }
    ));
    gateway.behaviour(|b| b.ignore_heartbeats = false);
    tunnel
        .wait_for(|e| matches!(e, Event::Connected { .. }), WAIT)
        .expect("reconnects");
    assert_eq!(
        gateway.tunnel_count(),
        1,
        "old tunnel was disconnected, new one open"
    );
}

#[test]
fn lost_ack_is_repeated() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut tunnel = connected(&gateway);
    gateway.behaviour(|b| b.drop_acks = 1);
    let id = tunnel.send(&write(1)).unwrap();
    assert_eq!(tunnel.wait_for_send(id, WAIT), Some(Ok(())));
}

#[test]
fn two_lost_acks_tear_down_and_reconnect() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut tunnel = connected(&gateway);
    gateway.behaviour(|b| b.drop_acks = 2);
    let id = tunnel.send(&write(1)).unwrap();
    assert_eq!(
        tunnel.wait_for_send(id, WAIT),
        Some(Err(SendError::NotAcknowledged))
    );
    tunnel
        .wait_for(|e| matches!(e, Event::Connected { .. }), WAIT)
        .expect("reconnects");
    let id = tunnel.send(&write(0)).unwrap();
    assert_eq!(tunnel.wait_for_send(id, WAIT), Some(Ok(())));
}

#[test]
fn negative_confirmation() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut tunnel = connected(&gateway);
    gateway.behaviour(|b| b.confirm_errors = true);
    let id = tunnel.send(&write(1)).unwrap();
    assert_eq!(
        tunnel.wait_for_send(id, WAIT),
        Some(Err(SendError::NegativeConfirmation))
    );
}

#[test]
fn refused_connection_is_retried() {
    let gateway = FakeGateway::spawn().unwrap();
    gateway.behaviour(|b| b.refuse_connections = Some(Status::NO_MORE_CONNECTIONS));
    let mut tunnel = BlockingTunnel::connect(fast_config(&gateway)).unwrap();
    let event = tunnel
        .wait_for(|e| matches!(e, Event::Disconnected { .. }), WAIT)
        .unwrap();
    assert!(matches!(
        event,
        Event::Disconnected {
            reason: DisconnectReason::ConnectRejected(Status::NO_MORE_CONNECTIONS),
            ..
        }
    ));
    gateway.behaviour(|b| b.refuse_connections = None);
    tunnel
        .wait_for(|e| matches!(e, Event::Connected { .. }), WAIT)
        .expect("connects once allowed");
}

#[test]
fn server_disconnect_and_gateway_restart() {
    let gateway = FakeGateway::spawn().unwrap();
    let address = gateway.address();
    let mut tunnel = connected(&gateway);
    gateway.disconnect_all();
    let event = tunnel
        .wait_for(|e| matches!(e, Event::Disconnected { .. }), WAIT)
        .unwrap();
    assert!(matches!(
        event,
        Event::Disconnected {
            reason: DisconnectReason::ServerDisconnected,
            ..
        }
    ));
    tunnel
        .wait_for(|e| matches!(e, Event::Connected { .. }), WAIT)
        .expect("reconnects");

    // Restart: the new gateway knows nothing about the old channel.
    gateway.stop();
    let _restarted = FakeGateway::bind(address).unwrap();
    let event = tunnel
        .wait_for(|e| matches!(e, Event::Disconnected { .. }), WAIT)
        .unwrap();
    assert!(matches!(
        event,
        Event::Disconnected {
            reason: DisconnectReason::HeartbeatFailed,
            ..
        }
    ));
    tunnel
        .wait_for(|e| matches!(e, Event::Connected { .. }), WAIT)
        .expect("reconnects after restart");
    let id = tunnel.send(&write(1)).unwrap();
    assert_eq!(tunnel.wait_for_send(id, WAIT), Some(Ok(())));
}

#[test]
fn close_disconnects() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut tunnel = connected(&gateway);
    tunnel.close();
    assert_eq!(
        tunnel.wait_for(|e| matches!(e, Event::Closed), WAIT),
        Some(Event::Closed)
    );
    assert_eq!(gateway.tunnel_count(), 0);
}

#[test]
fn two_tunnels_see_each_other() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut a = connected(&gateway);
    let mut b = connected(&gateway);
    let id = a.send(&write(1)).unwrap();
    assert_eq!(a.wait_for_send(id, WAIT), Some(Ok(())));
    let event = b
        .wait_for(|e| matches!(e, Event::Received(_)), WAIT)
        .unwrap();
    let Event::Received(Cemi::LDataInd(ldata)) = event else {
        panic!()
    };
    assert_eq!(ldata.source, a.client.address().unwrap());
}
