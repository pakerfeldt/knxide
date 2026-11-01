//! Tunnel server tests: real `TunnelClient`s talk to a `TunnelServer` in memory, on a fake
//! clock.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use knxide_core::{Apdu, Cemi, IndividualAddress, LData, Payload, ga};
use knxide_ip::Frame;
use knxide_ip::discovery::Discovery;
use knxide_ip::frame::Status;
use knxide_ip::server::{ClientGone, ServerConfig, ServerEvent, TunnelServer};
use knxide_ip::structures::{Cri, Hpai, TunnelLayer};
use knxide_ip::tunnel::{DisconnectReason, Event, TunnelClient, TunnelConfig};

const SERVER: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 148), 3671);
const SECOND: Duration = Duration::from_secs(1);

fn client_addr(n: u8) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10 + n), 50000))
}

fn ia(s: &str) -> IndividualAddress {
    s.parse().unwrap()
}

/// (from, to, frame) -> lose it?
type DropRule = Box<dyn Fn(SocketAddr, SocketAddr, &Frame) -> bool>;

struct Net {
    server: TunnelServer,
    clients: Vec<(SocketAddr, TunnelClient, Vec<Event>)>,
    now: Instant,
    /// Drop datagrams for which this returns true.
    drop: DropRule,
    server_events: Vec<ServerEvent>,
}

impl Net {
    fn new(config: ServerConfig) -> Self {
        Self {
            server: TunnelServer::new(config),
            clients: Vec::new(),
            now: Instant::now(),
            drop: Box::new(|_, _, _| false),
            server_events: Vec::new(),
        }
    }

    fn add_client(&mut self) -> usize {
        let n = self.clients.len();
        let client = TunnelClient::new(TunnelConfig::new(SocketAddr::V4(SERVER)), self.now);
        self.clients
            .push((client_addr(n as u8), client, Vec::new()));
        self.pump();
        n
    }

    /// Deliver datagrams until everything is quiet.
    fn pump(&mut self) {
        for _ in 0..1000 {
            let mut moved = false;
            for (addr, client, events) in &mut self.clients {
                while let Some(tx) = client.poll_transmit() {
                    moved = true;
                    let frame = Frame::decode(&tx.payload).unwrap();
                    if !(self.drop)(*addr, tx.destination, &frame) {
                        self.server.handle_input(*addr, &tx.payload, self.now);
                    }
                }
                events.extend(std::iter::from_fn(|| client.poll_event()));
            }
            while let Some(tx) = self.server.poll_transmit() {
                moved = true;
                let frame = Frame::decode(&tx.payload).unwrap();
                if let Some((addr, client, _)) =
                    self.clients.iter_mut().find(|(a, ..)| *a == tx.destination)
                    && !(self.drop)(SocketAddr::V4(SERVER), *addr, &frame)
                {
                    client.handle_input(SocketAddr::V4(SERVER), &tx.payload, self.now);
                }
            }
            self.server_events
                .extend(std::iter::from_fn(|| self.server.poll_event()));
            if !moved {
                return;
            }
        }
        panic!("network did not settle");
    }

    fn advance(&mut self, by: Duration) {
        let target = self.now + by;
        loop {
            let next = std::iter::once(self.server.poll_timeout())
                .chain(self.clients.iter().map(|(_, c, _)| c.poll_timeout()))
                .flatten()
                .filter(|t| *t <= target)
                .min();
            let Some(at) = next else { break };
            self.now = self.now.max(at);
            self.server.handle_timeout(self.now);
            for (_, client, _) in &mut self.clients {
                if client.poll_timeout().is_some_and(|t| t <= self.now) {
                    client.handle_timeout(self.now);
                }
            }
            self.pump();
        }
        self.now = target;
    }

    fn events(&mut self, n: usize) -> Vec<Event> {
        std::mem::take(&mut self.clients[n].2)
    }

    fn server_events(&mut self) -> Vec<ServerEvent> {
        std::mem::take(&mut self.server_events)
    }
}

fn write(value: u8) -> Cemi {
    Cemi::LDataReq(LData::group(
        ga!(1 / 2 / 3),
        Apdu::GroupValueWrite(Payload::Small(value)),
    ))
}

#[test]
fn clients_get_their_own_addresses() {
    let mut net = Net::new(ServerConfig::new(SERVER));
    let a = net.add_client();
    let b = net.add_client();
    assert!(
        matches!(net.events(a).as_slice(), [Event::Connected { address, .. }] if *address == ia("15.15.241"))
    );
    assert!(
        matches!(net.events(b).as_slice(), [Event::Connected { address, .. }] if *address == ia("15.15.242"))
    );
    let events = net.server_events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        ServerEvent::Connected { channel: 1, .. }
    ));
    assert_eq!(net.server.clients().count(), 2);
}

#[test]
fn frames_both_ways() {
    let mut net = Net::new(ServerConfig::new(SERVER));
    let a = net.add_client();
    let b = net.add_client();
    net.events(a);
    net.events(b);
    net.server_events();

    // Client to server: acknowledged by the server, handed to the application.
    let id = net.clients[a].1.send(&write(1), net.now).unwrap();
    net.pump();
    let events = net.server_events();
    let [ServerEvent::Frame { channel: 1, cemi }] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(cemi, &write(1));
    // The application confirms it; the client's send completes.
    let Cemi::LDataReq(mut ldata) = cemi.clone() else {
        panic!()
    };
    ldata.source = net.server.client_address(1).unwrap();
    net.server
        .send(1, &Cemi::LDataCon(ldata.clone()), net.now)
        .unwrap();
    net.pump();
    assert_eq!(net.events(a), [Event::SendResult { id, result: Ok(()) }]);

    // Server to clients.
    let ind = Cemi::LDataInd(ldata);
    net.server.send_all(&ind, Some(1), net.now);
    net.pump();
    assert!(net.events(a).is_empty());
    assert_eq!(net.events(b), [Event::Received(ind.clone())]);

    // Many frames in a row arrive in order, each acknowledged.
    for v in 0..20 {
        let mut l = LData::group(
            ga!(0 / 0 / 1),
            Apdu::GroupValueWrite(Payload::Bytes(vec![v])),
        );
        l.source = ia("1.1.5");
        net.server.send_all(&Cemi::LDataInd(l), None, net.now);
    }
    net.pump();
    assert_eq!(net.events(a).len(), 20);
    assert_eq!(net.events(b).len(), 20);
}

#[test]
fn connection_limit_and_requested_addresses() {
    let mut config = ServerConfig::new(SERVER);
    config.client_addresses = vec![ia("1.1.240"), ia("1.1.241")];
    let mut net = Net::new(config);
    net.add_client();
    net.add_client();
    let c = net.add_client();
    assert!(matches!(
        net.events(c).as_slice(),
        [Event::Disconnected {
            reason: DisconnectReason::ConnectRejected(Status::NO_MORE_CONNECTIONS),
            ..
        }]
    ));

    // Tunnelling v2 extended CRI asking for a specific address.
    let mut net = Net::new(ServerConfig::new(SERVER));
    let request = |address: &str| {
        Frame::ConnectRequest {
            control_endpoint: Hpai::ROUTE_BACK,
            data_endpoint: Hpai::ROUTE_BACK,
            cri: Cri::Tunnel {
                layer: TunnelLayer::LinkLayer,
                address: Some(ia(address)),
            },
        }
        .to_vec()
        .unwrap()
    };
    let response =
        |net: &mut Net| Frame::decode(&net.server.poll_transmit().unwrap().payload).unwrap();
    net.server
        .handle_input(client_addr(0), &request("15.15.250"), net.now);
    assert!(matches!(
        response(&mut net),
        Frame::ConnectResponse { status: Status::NO_ERROR, crd: Some(knxide_ip::Crd::Tunnel { address }), .. } if address == ia("15.15.250")
    ));
    net.server
        .handle_input(client_addr(1), &request("15.15.250"), net.now);
    assert!(matches!(
        response(&mut net),
        Frame::ConnectResponse {
            status: Status::CONNECTION_IN_USE,
            ..
        }
    ));
    net.server
        .handle_input(client_addr(1), &request("1.1.1"), net.now);
    assert!(matches!(
        response(&mut net),
        Frame::ConnectResponse {
            status: Status::NO_TUNNELLING_ADDRESS,
            ..
        }
    ));
}

#[test]
fn busmonitor_is_refused() {
    let mut net = Net::new(ServerConfig::new(SERVER));
    let request = Frame::ConnectRequest {
        control_endpoint: Hpai::ROUTE_BACK,
        data_endpoint: Hpai::ROUTE_BACK,
        cri: Cri::Tunnel {
            layer: TunnelLayer::Busmonitor,
            address: None,
        },
    };
    net.server
        .handle_input(client_addr(0), &request.to_vec().unwrap(), net.now);
    let reply = Frame::decode(&net.server.poll_transmit().unwrap().payload).unwrap();
    assert!(matches!(
        reply,
        Frame::ConnectResponse {
            status: Status::TUNNELLING_LAYER,
            ..
        }
    ));
}

#[test]
fn heartbeats_keep_clients_and_silence_drops_them() {
    let mut net = Net::new(ServerConfig::new(SERVER));
    let a = net.add_client();
    net.server_events();
    net.advance(10 * 60 * SECOND);
    assert_eq!(
        net.server.clients().count(),
        1,
        "client heartbeats every 60 s"
    );
    assert!(matches!(
        net.events(a).as_slice(),
        [Event::Connected { .. }]
    ));

    // The client vanishes: nothing it sends arrives any more.
    net.drop = Box::new(|from, _, _| from == client_addr(0));
    net.advance(121 * SECOND);
    assert!(net.server_events().contains(&ServerEvent::Disconnected {
        channel: 1,
        address: ia("15.15.241"),
        reason: ClientGone::HeartbeatTimeout
    }));
    assert_eq!(net.server.clients().count(), 0);
}

#[test]
fn unacknowledged_frames_are_repeated_once_then_the_client_is_dropped() {
    let mut net = Net::new(ServerConfig::new(SERVER));
    let a = net.add_client();
    net.events(a);
    net.server_events();
    // Lose the client's acks.
    net.drop = Box::new(|_, _, frame| matches!(frame, Frame::TunnellingAck { .. }));
    let ind = Cemi::LDataInd(LData::group(
        ga!(0 / 0 / 1),
        Apdu::GroupValueWrite(Payload::Small(1)),
    ));
    net.server.send(1, &ind, net.now).unwrap();
    net.pump();
    // The client got it once; the repeat is recognised as a duplicate.
    net.advance(SECOND);
    assert_eq!(net.events(a), [Event::Received(ind)]);
    net.advance(SECOND);
    assert!(net.server_events().iter().any(|e| matches!(
        e,
        ServerEvent::Disconnected {
            reason: ClientGone::AckTimeout,
            ..
        }
    )));
    // The client was told.
    assert!(net.events(a).iter().any(|e| matches!(
        e,
        Event::Disconnected {
            reason: DisconnectReason::ServerDisconnected,
            ..
        }
    )));
}

#[test]
fn bus_down_is_reported_to_clients() {
    let mut net = Net::new(ServerConfig::new(SERVER));
    let a = net.add_client();
    net.events(a);
    net.server.set_bus_connected(false);
    // Three refused heartbeats make the client give up and retry; the retry is refused too.
    net.advance(60 * SECOND);
    let events = net.events(a);
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Disconnected {
                reason: DisconnectReason::HeartbeatFailed,
                ..
            }
        )),
        "{events:?}"
    );
    net.advance(2 * SECOND);
    assert!(net.events(a).iter().any(|e| matches!(
        e,
        Event::Disconnected {
            reason: DisconnectReason::ConnectRejected(Status::KNX_CONNECTION),
            ..
        }
    )));
    net.server.set_bus_connected(true);
    net.advance(5 * SECOND);
    assert!(
        net.events(a)
            .iter()
            .any(|e| matches!(e, Event::Connected { .. }))
    );
}

#[test]
fn client_disconnect_and_server_shutdown() {
    let mut net = Net::new(ServerConfig::new(SERVER));
    let a = net.add_client();
    let b = net.add_client();
    net.server_events();
    net.clients[a].1.close(net.now);
    net.pump();
    assert!(net.events(a).contains(&Event::Closed));
    assert_eq!(
        net.server_events(),
        [ServerEvent::Disconnected {
            channel: 1,
            address: ia("15.15.241"),
            reason: ClientGone::Disconnected
        }]
    );
    net.server.disconnect_all();
    net.pump();
    assert!(net.events(b).iter().any(|e| matches!(
        e,
        Event::Disconnected {
            reason: DisconnectReason::ServerDisconnected,
            ..
        }
    )));
}

#[test]
fn discovery_finds_the_server() {
    let mut net = Net::new(ServerConfig {
        name: "knxide proxy".into(),
        ..ServerConfig::new(SERVER)
    });
    let mut search = Discovery::search(Hpai::ROUTE_BACK, SECOND, net.now);
    let request = search.poll_transmit().unwrap();
    net.server
        .handle_input(client_addr(0), &request.payload, net.now);
    let reply = net.server.poll_transmit().unwrap();
    assert_eq!(reply.destination, client_addr(0));
    search.handle_input(SocketAddr::V4(SERVER), &reply.payload, net.now);
    let gateway = search.poll_gateway().unwrap();
    assert_eq!(gateway.name(), "knxide proxy");
    assert_eq!(gateway.control_endpoint, SocketAddr::V4(SERVER));
    assert!(gateway.supports_tunnelling());
}
