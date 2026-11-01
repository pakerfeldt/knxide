//! Tunnel state machine tests with a fake clock and a scripted gateway.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use knxide_core::{Apdu, Cemi, IndividualAddress, LData, Payload, ga};
use knxide_ip::structures::TunnelLayer;
use knxide_ip::tunnel::{
    Backoff, DisconnectReason, Event, SendError, SendId, TunnelClient, TunnelConfig,
};
use knxide_ip::{Crd, Cri, Frame, Hpai, Status};

const GATEWAY: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 50), 3671));
const DATA: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 50), 3672));
const CHANNEL: u8 = 7;
const SECOND: Duration = Duration::from_secs(1);

fn tunnel_address() -> IndividualAddress {
    "1.1.250".parse().unwrap()
}

struct Harness {
    tunnel: TunnelClient,
    now: Instant,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(TunnelConfig::new(GATEWAY))
    }

    fn with_config(config: TunnelConfig) -> Self {
        let now = Instant::now();
        Self {
            tunnel: TunnelClient::new(config, now),
            now,
        }
    }

    /// A harness with an open tunnel and nothing left to drain.
    fn connected() -> Self {
        let mut h = Self::new();
        h.accept_connect();
        h
    }

    /// Answer the pending connect request.
    fn accept_connect(&mut self) {
        let sent = self.sent();
        assert!(
            matches!(sent.as_slice(), [(GATEWAY, Frame::ConnectRequest { .. })]),
            "expected a connect request, got {sent:?}"
        );
        self.receive_from(
            GATEWAY,
            Frame::ConnectResponse {
                channel_id: CHANNEL,
                status: Status::NO_ERROR,
                data_endpoint: Some(Hpai::udp(match DATA {
                    SocketAddr::V4(a) => a,
                    SocketAddr::V6(_) => unreachable!(),
                })),
                crd: Some(Crd::Tunnel {
                    address: tunnel_address(),
                }),
            },
        );
        assert_eq!(
            self.events(),
            [Event::Connected {
                address: tunnel_address(),
                channel_id: CHANNEL
            }]
        );
    }

    fn sent(&mut self) -> Vec<(SocketAddr, Frame)> {
        std::iter::from_fn(|| self.tunnel.poll_transmit())
            .map(|tx| (tx.destination, Frame::decode(&tx.payload).unwrap()))
            .collect()
    }

    fn events(&mut self) -> Vec<Event> {
        std::iter::from_fn(|| self.tunnel.poll_event()).collect()
    }

    fn receive_from(&mut self, source: SocketAddr, frame: Frame) {
        self.tunnel
            .handle_input(source, &frame.to_vec().unwrap(), self.now);
    }

    fn receive(&mut self, frame: Frame) {
        self.receive_from(DATA, frame);
    }

    /// Move the clock forward, firing every timer that comes due on the way.
    fn advance(&mut self, by: Duration) {
        let target = self.now + by;
        while let Some(deadline) = self.tunnel.poll_timeout() {
            if deadline > target {
                break;
            }
            self.now = self.now.max(deadline);
            self.tunnel.handle_timeout(self.now);
        }
        self.now = target;
    }

    fn send(&mut self, cemi: &Cemi) -> SendId {
        self.tunnel.send(cemi, self.now).unwrap()
    }

    fn ack(&mut self, sequence: u8) {
        self.receive(Frame::TunnellingAck {
            channel_id: CHANNEL,
            sequence,
            status: Status::NO_ERROR,
        });
    }

    /// The server's tunnelling request carrying `cemi`, with `sequence`.
    fn server_request(&mut self, sequence: u8, cemi: &Cemi) {
        self.receive(Frame::TunnellingRequest {
            channel_id: CHANNEL,
            sequence,
            cemi: cemi.to_vec().unwrap(),
        });
    }
}

fn write(value: u8) -> Cemi {
    Cemi::LDataReq(LData::group(
        ga!(1 / 2 / 3),
        Apdu::GroupValueWrite(Payload::Small(value)),
    ))
}

fn confirmation(error: bool) -> Cemi {
    let mut ldata = LData::group(ga!(1 / 2 / 3), Apdu::GroupValueWrite(Payload::Small(1)));
    ldata.source = tunnel_address();
    ldata.confirm_error = error;
    Cemi::LDataCon(ldata)
}

fn indication(value: u8) -> Cemi {
    let mut ldata = LData::group(ga!(0 / 0 / 1), Apdu::GroupValueWrite(Payload::Small(value)));
    ldata.source = "1.1.5".parse().unwrap();
    Cemi::LDataInd(ldata)
}

fn ack_frame(sequence: u8) -> Frame {
    Frame::TunnellingAck {
        channel_id: CHANNEL,
        sequence,
        status: Status::NO_ERROR,
    }
}

#[test]
fn connect_request_uses_route_back_and_link_layer() {
    let mut h = Harness::new();
    assert_eq!(
        h.sent(),
        [(
            GATEWAY,
            Frame::ConnectRequest {
                control_endpoint: Hpai::ROUTE_BACK,
                data_endpoint: Hpai::ROUTE_BACK,
                cri: Cri::Tunnel {
                    layer: TunnelLayer::LinkLayer,
                    address: None
                },
            }
        )]
    );
    assert_eq!(h.tunnel.poll_timeout(), Some(h.now + 10 * SECOND));
}

#[test]
fn connect_ok() {
    let h = Harness::connected();
    assert!(h.tunnel.is_connected());
    assert_eq!(h.tunnel.address(), Some(tunnel_address()));
    // Next wake-up is the heartbeat.
    assert_eq!(h.tunnel.poll_timeout(), Some(h.now + 60 * SECOND));
}

#[test]
fn route_back_data_endpoint_resolves_to_source() {
    let mut h = Harness::new();
    h.sent();
    h.receive_from(
        GATEWAY,
        Frame::ConnectResponse {
            channel_id: CHANNEL,
            status: Status::NO_ERROR,
            data_endpoint: Some(Hpai::ROUTE_BACK),
            crd: Some(Crd::Tunnel {
                address: tunnel_address(),
            }),
        },
    );
    h.send(&write(1));
    assert!(matches!(
        h.sent().as_slice(),
        [(GATEWAY, Frame::TunnellingRequest { .. })]
    ));
}

#[test]
fn connect_rejected_for_each_error_status() {
    for status in [
        Status::CONNECTION_TYPE,
        Status::CONNECTION_OPTION,
        Status::NO_MORE_CONNECTIONS,
        Status::TUNNELLING_LAYER,
        Status::NO_TUNNELLING_ADDRESS,
        Status::CONNECTION_IN_USE,
        Status(0x99),
    ] {
        let mut h = Harness::new();
        h.sent();
        h.receive_from(
            GATEWAY,
            Frame::ConnectResponse {
                channel_id: 0,
                status,
                data_endpoint: None,
                crd: None,
            },
        );
        assert_eq!(
            h.events(),
            [Event::Disconnected {
                reason: DisconnectReason::ConnectRejected(status),
                reconnect_at: Some(h.now + SECOND),
            }],
            "{status}"
        );
        assert!(!h.tunnel.is_connected());
    }
}

#[test]
fn connect_timeout_and_backoff() {
    let mut h = Harness::new();
    h.sent();
    let mut expected_delay = SECOND;
    for _ in 0..8 {
        h.advance(10 * SECOND);
        let events = h.events();
        assert_eq!(
            events,
            [Event::Disconnected {
                reason: DisconnectReason::ConnectTimeout,
                reconnect_at: Some(h.now + expected_delay),
            }]
        );
        assert!(h.sent().is_empty());
        h.advance(expected_delay);
        assert!(matches!(
            h.sent().as_slice(),
            [(GATEWAY, Frame::ConnectRequest { .. })]
        ));
        expected_delay = (expected_delay * 2).min(60 * SECOND);
    }
    // A successful connect resets the back-off.
    h.receive_from(
        GATEWAY,
        Frame::ConnectResponse {
            channel_id: CHANNEL,
            status: Status::NO_ERROR,
            data_endpoint: Some(Hpai::ROUTE_BACK),
            crd: Some(Crd::Tunnel {
                address: tunnel_address(),
            }),
        },
    );
    h.events();
    h.receive_from(
        GATEWAY,
        Frame::DisconnectRequest {
            channel_id: CHANNEL,
            control_endpoint: Hpai::ROUTE_BACK,
        },
    );
    assert!(matches!(
        h.events().as_slice(),
        [Event::Disconnected { reconnect_at: Some(at), .. }] if *at == h.now + SECOND
    ));
}

#[test]
fn invalid_connect_response() {
    let mut h = Harness::new();
    h.sent();
    h.receive_from(
        GATEWAY,
        Frame::ConnectResponse {
            channel_id: CHANNEL,
            status: Status::NO_ERROR,
            data_endpoint: Some(Hpai::ROUTE_BACK),
            crd: Some(Crd::Other {
                connection_type: 3,
                data: vec![],
            }),
        },
    );
    assert!(matches!(
        h.events().as_slice(),
        [Event::Disconnected {
            reason: DisconnectReason::InvalidConnectResponse,
            ..
        }]
    ));
}

#[test]
fn heartbeat_success() {
    let mut h = Harness::connected();
    for _ in 0..3 {
        h.advance(59 * SECOND);
        assert!(h.sent().is_empty());
        h.advance(SECOND);
        assert_eq!(
            h.sent(),
            [(
                GATEWAY,
                Frame::ConnectionStateRequest {
                    channel_id: CHANNEL,
                    control_endpoint: Hpai::ROUTE_BACK
                }
            )]
        );
        h.advance(SECOND);
        h.receive_from(
            GATEWAY,
            Frame::ConnectionStateResponse {
                channel_id: CHANNEL,
                status: Status::NO_ERROR,
            },
        );
        // Next heartbeat is a full interval after the response.
        assert_eq!(h.tunnel.poll_timeout(), Some(h.now + 60 * SECOND));
    }
    assert!(h.events().is_empty());
}

#[test]
fn heartbeat_fails_three_times() {
    let mut h = Harness::connected();
    h.advance(60 * SECOND);
    assert_eq!(h.sent().len(), 1);
    h.advance(10 * SECOND);
    assert_eq!(h.sent().len(), 1, "second attempt");
    h.advance(10 * SECOND);
    assert_eq!(h.sent().len(), 1, "third attempt");
    assert!(h.events().is_empty());
    h.advance(10 * SECOND);
    assert_eq!(
        h.sent(),
        [(
            GATEWAY,
            Frame::DisconnectRequest {
                channel_id: CHANNEL,
                control_endpoint: Hpai::ROUTE_BACK
            }
        )]
    );
    assert_eq!(
        h.events(),
        [Event::Disconnected {
            reason: DisconnectReason::HeartbeatFailed,
            reconnect_at: Some(h.now + SECOND)
        }]
    );
}

#[test]
fn heartbeat_error_status_counts_as_failure() {
    let mut h = Harness::connected();
    h.advance(60 * SECOND);
    h.sent();
    for attempt in 1..=3 {
        h.receive_from(
            GATEWAY,
            Frame::ConnectionStateResponse {
                channel_id: CHANNEL,
                status: Status::KNX_CONNECTION,
            },
        );
        let sent = h.sent();
        if attempt < 3 {
            assert!(matches!(
                sent.as_slice(),
                [(GATEWAY, Frame::ConnectionStateRequest { .. })]
            ));
        } else {
            assert!(matches!(
                sent.as_slice(),
                [(GATEWAY, Frame::DisconnectRequest { .. })]
            ));
        }
    }
    assert!(matches!(
        h.events().as_slice(),
        [Event::Disconnected {
            reason: DisconnectReason::HeartbeatFailed,
            ..
        }]
    ));
}

#[test]
fn send_waits_for_ack_and_confirmation() {
    let mut h = Harness::connected();
    let first = h.send(&write(1));
    let second = h.send(&write(0));
    let sent = h.sent();
    assert_eq!(sent.len(), 1, "only one L_Data.req outstanding");
    assert!(matches!(
        &sent[0],
        (
            DATA,
            Frame::TunnellingRequest {
                channel_id: CHANNEL,
                sequence: 0,
                ..
            }
        )
    ));

    h.ack(0);
    assert!(h.sent().is_empty(), "still waiting for L_Data.con");
    assert!(h.events().is_empty());

    h.server_request(0, &confirmation(false));
    assert_eq!(h.sent()[0], (DATA, ack_frame(0)));
    assert_eq!(
        h.events(),
        [Event::SendResult {
            id: first,
            result: Ok(())
        }]
    );
    // The confirmation released the second frame, with the next sequence number.
    h.receive(Frame::TunnellingAck {
        channel_id: CHANNEL,
        sequence: 1,
        status: Status::NO_ERROR,
    });
    h.server_request(1, &confirmation(false));
    assert_eq!(
        h.events(),
        [Event::SendResult {
            id: second,
            result: Ok(())
        }]
    );
}

#[test]
fn second_frame_is_sent_after_confirmation() {
    let mut h = Harness::connected();
    h.send(&write(1));
    h.send(&write(0));
    h.sent();
    h.ack(0);
    h.server_request(0, &confirmation(false));
    let sent = h.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], (DATA, ack_frame(0)));
    let (
        _,
        Frame::TunnellingRequest {
            sequence: 1, cemi, ..
        },
    ) = &sent[1]
    else {
        panic!("{sent:?}")
    };
    assert_eq!(Cemi::decode(cemi), Ok(write(0)));
}

#[test]
fn ack_timeout_resends_once_then_tears_down() {
    let mut h = Harness::connected();
    let id = h.send(&write(1));
    let queued = h.send(&write(0));
    let first = h.sent();
    h.advance(SECOND);
    let repeat = h.sent();
    assert_eq!(first, repeat, "repeated with the same sequence number");
    h.advance(SECOND);
    assert_eq!(
        h.sent(),
        [(
            GATEWAY,
            Frame::DisconnectRequest {
                channel_id: CHANNEL,
                control_endpoint: Hpai::ROUTE_BACK
            }
        )]
    );
    assert_eq!(
        h.events(),
        [
            Event::SendResult {
                id,
                result: Err(SendError::NotAcknowledged)
            },
            Event::Disconnected {
                reason: DisconnectReason::AckTimeout,
                reconnect_at: Some(h.now + SECOND)
            },
        ]
    );
    // The queued frame survives and goes out after reconnecting, with sequence 0.
    h.advance(SECOND);
    h.accept_connect();
    let sent = h.sent();
    let [
        (
            DATA,
            Frame::TunnellingRequest {
                sequence: 0, cemi, ..
            },
        ),
    ] = sent.as_slice()
    else {
        panic!("{sent:?}")
    };
    assert_eq!(Cemi::decode(cemi), Ok(write(0)));
    h.ack(0);
    h.server_request(0, &confirmation(false));
    assert_eq!(
        h.events(),
        [Event::SendResult {
            id: queued,
            result: Ok(())
        }]
    );
}

#[test]
fn error_ack_is_repeated_like_a_timeout() {
    let mut h = Harness::connected();
    h.send(&write(1));
    h.sent();
    h.receive(Frame::TunnellingAck {
        channel_id: CHANNEL,
        sequence: 0,
        status: Status::ERROR,
    });
    assert!(matches!(
        h.sent().as_slice(),
        [(DATA, Frame::TunnellingRequest { sequence: 0, .. })]
    ));
    h.receive(Frame::TunnellingAck {
        channel_id: CHANNEL,
        sequence: 0,
        status: Status::ERROR,
    });
    assert!(matches!(
        h.sent().as_slice(),
        [(GATEWAY, Frame::DisconnectRequest { .. })]
    ));
}

#[test]
fn acks_for_other_sequences_or_channels_are_ignored() {
    let mut h = Harness::connected();
    let id = h.send(&write(1));
    h.sent();
    h.ack(5);
    h.receive(Frame::TunnellingAck {
        channel_id: CHANNEL + 1,
        sequence: 0,
        status: Status::NO_ERROR,
    });
    h.advance(SECOND);
    assert_eq!(h.sent().len(), 1, "still unacknowledged, so repeated");
    h.ack(0);
    h.server_request(0, &confirmation(false));
    assert_eq!(h.events(), [Event::SendResult { id, result: Ok(()) }]);
}

#[test]
fn negative_confirmation() {
    let mut h = Harness::connected();
    let id = h.send(&write(1));
    h.sent();
    h.ack(0);
    h.server_request(0, &confirmation(true));
    assert_eq!(
        h.events(),
        [Event::SendResult {
            id,
            result: Err(SendError::NegativeConfirmation)
        }]
    );
    assert!(h.tunnel.is_connected());
}

#[test]
fn confirmation_timeout() {
    let mut h = Harness::connected();
    let id = h.send(&write(1));
    let next = h.send(&write(0));
    h.sent();
    h.ack(0);
    h.advance(3 * SECOND);
    assert_eq!(
        h.events(),
        [Event::SendResult {
            id,
            result: Err(SendError::ConfirmationTimeout)
        }]
    );
    assert!(matches!(
        h.sent().as_slice(),
        [(DATA, Frame::TunnellingRequest { sequence: 1, .. })]
    ));
    h.ack(1);
    h.server_request(0, &confirmation(false));
    assert_eq!(
        h.events(),
        [Event::SendResult {
            id: next,
            result: Ok(())
        }]
    );
}

#[test]
fn confirmation_before_ack() {
    let mut h = Harness::connected();
    let id = h.send(&write(1));
    h.sent();
    h.server_request(0, &confirmation(false));
    assert!(h.events().is_empty());
    h.ack(0);
    assert_eq!(h.events(), [Event::SendResult { id, result: Ok(()) }]);
}

#[test]
fn non_ldata_requests_complete_on_ack() {
    let mut h = Harness::connected();
    let id = h.send(&Cemi::Other {
        message_code: 0xFC,
        data: vec![0, 0, 1, 1, 0x34, 0x10, 0x01],
    });
    h.sent();
    h.ack(0);
    assert_eq!(h.events(), [Event::SendResult { id, result: Ok(()) }]);
}

#[test]
fn inbound_frames_are_acknowledged_in_sequence() {
    let mut h = Harness::connected();
    h.server_request(0, &indication(1));
    assert_eq!(h.sent(), [(DATA, ack_frame(0))]);
    assert_eq!(h.events(), [Event::Received(indication(1))]);

    // Repeat of the last one: acknowledge again, do not deliver twice.
    h.server_request(0, &indication(1));
    assert_eq!(h.sent(), [(DATA, ack_frame(0))]);
    assert!(h.events().is_empty());

    // Out of order: drop without an ack.
    h.server_request(5, &indication(0));
    assert!(h.sent().is_empty());
    assert!(h.events().is_empty());

    // The expected frame still works and cancels the out-of-sequence timer.
    h.server_request(1, &indication(0));
    assert_eq!(h.sent(), [(DATA, ack_frame(1))]);
    assert_eq!(h.events(), [Event::Received(indication(0))]);
    h.advance(5 * SECOND);
    assert!(h.tunnel.is_connected());
}

#[test]
fn persistent_out_of_sequence_tears_down() {
    let mut h = Harness::connected();
    h.server_request(3, &indication(1));
    h.advance(SECOND);
    h.server_request(4, &indication(1));
    assert!(h.tunnel.is_connected());
    h.advance(SECOND);
    assert!(matches!(
        h.sent().as_slice(),
        [(GATEWAY, Frame::DisconnectRequest { .. })]
    ));
    assert!(matches!(
        h.events().as_slice(),
        [Event::Disconnected {
            reason: DisconnectReason::SequenceError,
            ..
        }]
    ));
}

#[test]
fn frames_for_other_channels_are_ignored() {
    let mut h = Harness::connected();
    h.receive(Frame::TunnellingRequest {
        channel_id: CHANNEL + 1,
        sequence: 0,
        cemi: indication(1).to_vec().unwrap(),
    });
    assert!(h.sent().is_empty());
    assert!(h.events().is_empty());
}

#[test]
fn undecodable_cemi_is_still_acknowledged() {
    let mut h = Harness::connected();
    h.receive(Frame::TunnellingRequest {
        channel_id: CHANNEL,
        sequence: 0,
        cemi: vec![0x29, 0x00],
    });
    assert_eq!(h.sent(), [(DATA, ack_frame(0))]);
    assert!(h.events().is_empty());
    h.tunnel.handle_input(DATA, &[0x06, 0x10, 0xFF], h.now);
    assert!(h.sent().is_empty());
}

#[test]
fn server_initiated_disconnect() {
    let mut h = Harness::connected();
    let id = h.send(&write(1));
    h.sent();
    let client_endpoint = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 50), 3671);
    h.receive_from(
        GATEWAY,
        Frame::DisconnectRequest {
            channel_id: CHANNEL,
            control_endpoint: Hpai::udp(client_endpoint),
        },
    );
    assert_eq!(
        h.sent(),
        [(
            GATEWAY,
            Frame::DisconnectResponse {
                channel_id: CHANNEL,
                status: Status::NO_ERROR
            }
        )]
    );
    assert_eq!(
        h.events(),
        [
            Event::SendResult {
                id,
                result: Err(SendError::ConnectionLost)
            },
            Event::Disconnected {
                reason: DisconnectReason::ServerDisconnected,
                reconnect_at: Some(h.now + SECOND)
            },
        ]
    );
    h.advance(SECOND);
    h.accept_connect();
}

#[test]
fn disconnect_for_other_channel_is_ignored() {
    let mut h = Harness::connected();
    h.receive_from(
        GATEWAY,
        Frame::DisconnectRequest {
            channel_id: CHANNEL + 1,
            control_endpoint: Hpai::ROUTE_BACK,
        },
    );
    assert!(h.sent().is_empty());
    assert!(h.tunnel.is_connected());
}

#[test]
fn outbound_sequence_wraps() {
    let mut h = Harness::connected();
    for i in 0..300u32 {
        let id = h.send(&write((i % 2) as u8));
        let sent = h.sent();
        let [(DATA, Frame::TunnellingRequest { sequence, .. })] = sent.as_slice() else {
            panic!("{sent:?}")
        };
        assert_eq!(u32::from(*sequence), i % 256);
        h.ack(*sequence);
        h.server_request((i % 256) as u8, &confirmation(false));
        h.sent();
        assert_eq!(h.events(), [Event::SendResult { id, result: Ok(()) }]);
    }
}

#[test]
fn inbound_sequence_wraps() {
    let mut h = Harness::connected();
    for i in 0..300u32 {
        h.server_request((i % 256) as u8, &indication(1));
        assert_eq!(h.sent(), [(DATA, ack_frame((i % 256) as u8))]);
        assert_eq!(h.events().len(), 1, "frame {i}");
    }
    // 299 % 256 = 43: a repeat of 43 is acknowledged, 42 is out of sequence.
    h.server_request(43, &indication(1));
    assert_eq!(h.sent().len(), 1);
    h.server_request(42, &indication(1));
    assert!(h.sent().is_empty());
}

#[test]
fn close() {
    let mut h = Harness::connected();
    let in_flight = h.send(&write(1));
    let queued = h.send(&write(0));
    h.sent();
    h.tunnel.close(h.now);
    assert_eq!(
        h.sent(),
        [(
            GATEWAY,
            Frame::DisconnectRequest {
                channel_id: CHANNEL,
                control_endpoint: Hpai::ROUTE_BACK
            }
        )]
    );
    assert_eq!(
        h.events(),
        [
            Event::SendResult {
                id: in_flight,
                result: Err(SendError::Closed)
            },
            Event::SendResult {
                id: queued,
                result: Err(SendError::Closed)
            },
        ]
    );
    assert_eq!(h.tunnel.send(&write(1), h.now), Err(SendError::Closed));
    h.receive_from(
        GATEWAY,
        Frame::DisconnectResponse {
            channel_id: CHANNEL,
            status: Status::NO_ERROR,
        },
    );
    assert_eq!(h.events(), [Event::Closed]);
    assert!(h.tunnel.is_closed());
    assert_eq!(h.tunnel.poll_timeout(), None);
}

#[test]
fn close_without_response_times_out() {
    let mut h = Harness::connected();
    h.tunnel.close(h.now);
    h.sent();
    h.advance(SECOND);
    assert_eq!(h.events(), [Event::Closed]);
}

#[test]
fn close_while_connecting() {
    let mut h = Harness::new();
    let id = h.send(&write(1));
    h.tunnel.close(h.now);
    assert_eq!(
        h.events(),
        [
            Event::SendResult {
                id,
                result: Err(SendError::Closed)
            },
            Event::Closed
        ]
    );
}

#[test]
fn without_reconnect_a_lost_connection_closes() {
    let mut config = TunnelConfig::new(GATEWAY);
    config.reconnect = None;
    let mut h = Harness::with_config(config);
    h.accept_connect();
    h.send(&write(1));
    let queued = h.send(&write(0));
    h.receive_from(
        GATEWAY,
        Frame::DisconnectRequest {
            channel_id: CHANNEL,
            control_endpoint: Hpai::ROUTE_BACK,
        },
    );
    let events = h.events();
    assert!(events.contains(&Event::Disconnected {
        reason: DisconnectReason::ServerDisconnected,
        reconnect_at: None
    }));
    assert!(events.contains(&Event::SendResult {
        id: queued,
        result: Err(SendError::Closed)
    }));
    assert_eq!(events.last(), Some(&Event::Closed));
}

#[test]
fn queue_limit() {
    let mut config = TunnelConfig::new(GATEWAY);
    config.max_queue_len = 2;
    let mut h = Harness::with_config(config);
    h.send(&write(1));
    h.send(&write(1));
    assert_eq!(h.tunnel.send(&write(1), h.now), Err(SendError::QueueFull));
}

#[test]
fn custom_backoff() {
    let mut config = TunnelConfig::new(GATEWAY);
    config.reconnect = Some(Backoff {
        initial: 5 * SECOND,
        max: 8 * SECOND,
    });
    let mut h = Harness::with_config(config);
    h.sent();
    for delay in [5, 8, 8] {
        h.advance(10 * SECOND);
        assert!(matches!(
            h.events().as_slice(),
            [Event::Disconnected { reconnect_at: Some(at), .. }] if *at == h.now + delay * SECOND
        ));
        h.advance(delay * SECOND);
        h.sent();
    }
}

mod tcp {
    use super::*;
    use knxide_ip::tunnel::Transport;

    fn tcp_harness() -> Harness {
        let mut config = TunnelConfig::new(GATEWAY);
        config.transport = Transport::Tcp;
        let mut h = Harness::with_config(config);
        let sent = h.sent();
        let [
            (
                GATEWAY,
                Frame::ConnectRequest {
                    control_endpoint,
                    data_endpoint,
                    ..
                },
            ),
        ] = sent.as_slice()
        else {
            panic!("{sent:?}")
        };
        assert_eq!(*control_endpoint, Hpai::TCP);
        assert_eq!(*data_endpoint, Hpai::TCP);
        h.receive_from(
            GATEWAY,
            Frame::ConnectResponse {
                channel_id: CHANNEL,
                status: Status::NO_ERROR,
                data_endpoint: Some(Hpai::TCP),
                crd: Some(Crd::Tunnel {
                    address: tunnel_address(),
                }),
            },
        );
        assert!(matches!(h.events().as_slice(), [Event::Connected { .. }]));
        h
    }

    #[test]
    fn sends_without_acks() {
        let mut h = tcp_harness();
        let first = h.send(&write(1));
        let second = h.send(&write(0));
        let sent = h.sent();
        assert!(
            matches!(
                sent.as_slice(),
                [(_, Frame::TunnellingRequest { sequence: 0, .. })]
            ),
            "{sent:?}"
        );
        // No ack needed: nothing is repeated, only the confirmation timeout applies.
        h.advance(2 * SECOND);
        assert!(h.sent().is_empty());
        h.advance(SECOND);
        assert_eq!(
            h.events(),
            [Event::SendResult {
                id: first,
                result: Err(SendError::ConfirmationTimeout)
            }]
        );
        let sent = h.sent();
        assert!(
            matches!(
                sent.as_slice(),
                [(_, Frame::TunnellingRequest { sequence: 1, .. })]
            ),
            "{sent:?}"
        );
        h.server_request(0, &confirmation(false));
        assert!(h.sent().is_empty(), "no ack over TCP");
        assert_eq!(
            h.events(),
            [Event::SendResult {
                id: second,
                result: Ok(())
            }]
        );
    }

    #[test]
    fn receives_without_acks_or_sequence_checks() {
        let mut h = tcp_harness();
        h.server_request(5, &indication(1));
        h.server_request(5, &indication(0));
        assert!(h.sent().is_empty());
        assert_eq!(
            h.events(),
            [
                Event::Received(indication(1)),
                Event::Received(indication(0))
            ]
        );
    }

    #[test]
    fn closed_stream_reconnects() {
        let mut h = tcp_harness();
        let id = h.send(&write(1));
        h.sent();
        h.tunnel.transport_closed(h.now);
        assert_eq!(
            h.events(),
            [
                Event::SendResult {
                    id,
                    result: Err(SendError::ConnectionLost)
                },
                Event::Disconnected {
                    reason: DisconnectReason::TransportClosed,
                    reconnect_at: Some(h.now + SECOND)
                },
            ]
        );
        h.advance(SECOND);
        assert!(matches!(
            h.sent().as_slice(),
            [(_, Frame::ConnectRequest { .. })]
        ));
    }

    #[test]
    fn heartbeat_still_runs() {
        let mut h = tcp_harness();
        h.advance(60 * SECOND);
        assert!(matches!(
            h.sent().as_slice(),
            [(
                _,
                Frame::ConnectionStateRequest {
                    control_endpoint: Hpai::TCP,
                    ..
                }
            )]
        ));
    }

    #[test]
    fn transport_closed_is_ignored_over_udp() {
        let mut h = Harness::connected();
        h.tunnel.transport_closed(h.now);
        assert!(h.tunnel.is_connected());
    }
}
