//! Sans-IO KNXnet/IP tunnelling client. Spec: 3/8/4 (Tunnelling), 3/8/2 (Core) §5 for the
//! connection lifecycle and heartbeat.
//!
//! The client never touches sockets or clocks. Feed it datagrams and the current time, then
//! drain what it wants to send, when it wants to be woken, and what happened:
//!
//! ```
//! # use std::time::Instant;
//! # use knxide_ip::tunnel::{TunnelClient, TunnelConfig};
//! let now = Instant::now();
//! let mut tunnel = TunnelClient::new(TunnelConfig::new("192.168.1.50:3671".parse().unwrap()), now);
//! while let Some(tx) = tunnel.poll_transmit() {
//!     // socket.send_to(&tx.payload, tx.destination)
//! #   let _ = tx;
//! }
//! // On a datagram:  tunnel.handle_input(source, &datagram, now);
//! // At the deadline from tunnel.poll_timeout():  tunnel.handle_timeout(now);
//! while let Some(event) = tunnel.poll_event() {
//!     // Connected, Disconnected, Received(cemi), SendResult { id, result }, Closed
//! #   let _ = event;
//! }
//! ```
//!
//! Behaviour, per spec unless noted:
//! - Connect, then send a heartbeat (`CONNECTIONSTATE_REQUEST`) every 60 s. Three failed
//!   heartbeats (10 s timeout each) end the connection.
//! - Each `TUNNELLING_REQUEST` must be acknowledged within 1 s. It is sent once more on
//!   timeout or an error ack, then the connection is torn down.
//! - Only one `L_Data.req` is outstanding at a time: the next is sent once the interface's
//!   `L_Data.con` arrives (or 3 s pass). This follows xknx and keeps from overrunning the
//!   interface; the spec only requires waiting for the ack.
//! - Inbound requests are acknowledged and processed when their sequence number is the
//!   expected one, acknowledged and dropped when it is one less (a repeat), and dropped
//!   otherwise. If no in-sequence frame follows within 2 s of an out-of-sequence one, the
//!   connection is torn down (xknx does this for servers that never give up).
//! - After a connection is lost, reconnect with exponential back-off.

use std::collections::VecDeque;
use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use knxide_core::{Cemi, EncodeError, IndividualAddress};

use crate::frame::{Frame, Status};
use crate::structures::{Crd, Cri, Hpai, TunnelLayer};

/// A datagram to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transmit {
    /// Where to send it.
    pub destination: SocketAddr,
    /// The encoded KNXnet/IP frame.
    pub payload: Vec<u8>,
}

/// Reconnect back-off: the first delay, doubled after each failure up to `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// Delay before the first reconnect attempt.
    pub initial: Duration,
    /// Upper bound for the delay.
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
        }
    }
}

/// How frames travel to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// UDP, with tunnelling acks and repeats. Works with every interface.
    #[default]
    Udp,
    /// TCP (tunnelling v2, 3/8/4 §2.6): no tunnelling acks, the stream is reliable. The
    /// caller owns the stream: open it before sending the first transmit, and report a closed
    /// stream with [`TunnelClient::transport_closed`].
    Tcp,
}

/// Tunnel settings. Defaults follow the spec (3/8/4 and 3/8/2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelConfig {
    /// The server's control endpoint.
    pub gateway: SocketAddr,
    /// UDP or TCP.
    pub transport: Transport,
    /// Our endpoint as advertised in the HPAIs. Default [`Hpai::ROUTE_BACK`] (NAT mode).
    /// Over TCP this is always [`Hpai::TCP`].
    pub local_endpoint: Hpai,
    /// KNX layer to open: link layer for normal use, busmonitor to watch raw traffic.
    pub layer: TunnelLayer,
    /// Ask for a specific tunnel address (tunnelling v2 servers only).
    pub requested_address: Option<IndividualAddress>,
    /// `CONNECT_REQUEST_TIMEOUT`: 10 s.
    pub connect_timeout: Duration,
    /// Interval between heartbeats: 60 s. The server drops idle connections after 120 s.
    pub heartbeat_interval: Duration,
    /// `CONNECTIONSTATE_REQUEST_TIMEOUT`: 10 s.
    pub heartbeat_timeout: Duration,
    /// Failed heartbeats before the connection is considered lost: 3.
    pub heartbeat_attempts: u8,
    /// `TUNNELLING_REQUEST_TIMEOUT`: 1 s.
    pub ack_timeout: Duration,
    /// How long to wait for `L_Data.con` after the ack: 3 s (xknx).
    pub confirmation_timeout: Duration,
    /// Tear down if an out-of-sequence frame is not followed by an in-sequence one: 2 s (xknx).
    pub out_of_sequence_timeout: Duration,
    /// How long `close` waits for the `DISCONNECT_RESPONSE`.
    pub disconnect_timeout: Duration,
    /// Reconnect after losing the connection. `None` closes instead.
    pub reconnect: Option<Backoff>,
    /// Most frames that may wait to be sent.
    pub max_queue_len: usize,
}

impl TunnelConfig {
    /// Defaults for a link-layer tunnel to `gateway`.
    pub fn new(gateway: SocketAddr) -> Self {
        Self {
            gateway,
            transport: Transport::Udp,
            local_endpoint: Hpai::ROUTE_BACK,
            layer: TunnelLayer::LinkLayer,
            requested_address: None,
            connect_timeout: Duration::from_secs(10),
            heartbeat_interval: Duration::from_secs(60),
            heartbeat_timeout: Duration::from_secs(10),
            heartbeat_attempts: 3,
            ack_timeout: Duration::from_secs(1),
            confirmation_timeout: Duration::from_secs(3),
            out_of_sequence_timeout: Duration::from_secs(2),
            disconnect_timeout: Duration::from_secs(1),
            reconnect: Some(Backoff::default()),
            max_queue_len: 256,
        }
    }
}

/// Identifies a frame passed to [`TunnelClient::send`] in its [`Event::SendResult`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SendId(pub u64);

/// Why a send failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// The frame could not be encoded.
    Encode(EncodeError),
    /// Too many frames are waiting.
    QueueFull,
    /// The client is closed or closing.
    Closed,
    /// The server did not acknowledge the frame, twice. The connection was torn down.
    NotAcknowledged,
    /// The connection was lost while the frame was in flight.
    ConnectionLost,
    /// No `L_Data.con` arrived in time. The frame may or may not have reached the bus.
    ConfirmationTimeout,
    /// The interface confirmed the frame with an error: it could not be sent on the bus.
    NegativeConfirmation,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(e) => write!(f, "could not encode frame: {e}"),
            Self::QueueFull => f.write_str("send queue is full"),
            Self::Closed => f.write_str("tunnel is closed"),
            Self::NotAcknowledged => f.write_str("server did not acknowledge the frame"),
            Self::ConnectionLost => f.write_str("connection lost before the frame was confirmed"),
            Self::ConfirmationTimeout => f.write_str("no confirmation from the interface"),
            Self::NegativeConfirmation => {
                f.write_str("interface could not send the frame on the bus")
            }
        }
    }
}

impl std::error::Error for SendError {}

/// Why the connection ended or could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// No `CONNECT_RESPONSE` in time.
    ConnectTimeout,
    /// The server refused the connection, e.g. [`Status::NO_MORE_CONNECTIONS`].
    ConnectRejected(Status),
    /// The server accepted but the response was unusable (no data endpoint or CRD).
    InvalidConnectResponse,
    /// Heartbeats failed.
    HeartbeatFailed,
    /// A tunnelling request was not acknowledged twice.
    AckTimeout,
    /// Inbound sequence numbers stayed out of order.
    SequenceError,
    /// The server sent `DISCONNECT_REQUEST`.
    ServerDisconnected,
    /// The TCP stream closed or could not be opened.
    TransportClosed,
}

impl fmt::Display for DisconnectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectTimeout => f.write_str("no response to connect request"),
            Self::ConnectRejected(status) => write!(f, "connection refused: {status}"),
            Self::InvalidConnectResponse => f.write_str("invalid connect response"),
            Self::HeartbeatFailed => f.write_str("heartbeat failed"),
            Self::AckTimeout => f.write_str("tunnelling request not acknowledged"),
            Self::SequenceError => f.write_str("inbound frames out of sequence"),
            Self::ServerDisconnected => f.write_str("server closed the connection"),
            Self::TransportClosed => f.write_str("TCP connection closed"),
        }
    }
}

/// Something that happened, drained with [`TunnelClient::poll_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The tunnel is open.
    Connected {
        /// Individual address assigned to the tunnel; frames we send appear from it.
        address: IndividualAddress,
        /// Channel id.
        channel_id: u8,
    },
    /// The tunnel closed or a connect attempt failed.
    Disconnected {
        /// Why.
        reason: DisconnectReason,
        /// When the next connect attempt happens, if reconnecting.
        reconnect_at: Option<Instant>,
    },
    /// A frame from the bus (normally `L_Data.ind`). Confirmations are reported as
    /// [`Event::SendResult`] instead.
    Received(Cemi),
    /// Outcome of a [`TunnelClient::send`]. For `L_Data.req`, success means the interface
    /// confirmed sending the frame on the bus.
    SendResult {
        /// The id returned by `send`.
        id: SendId,
        /// The outcome.
        result: Result<(), SendError>,
    },
    /// The client is closed and will do nothing more.
    Closed,
}

#[derive(Debug)]
struct Pending {
    id: SendId,
    cemi: Vec<u8>,
    /// `L_Data.req` gets an `L_Data.con`; other messages complete on the ack.
    expects_confirmation: bool,
}

#[derive(Debug)]
enum Phase {
    AwaitAck { deadline: Instant, attempt: u8 },
    AwaitConfirmation { deadline: Instant },
}

#[derive(Debug)]
struct InFlight {
    pending: Pending,
    sequence: u8,
    phase: Phase,
    /// An `L_Data.con` that overtook the ack: `Some(error flag)`.
    early_confirmation: Option<bool>,
}

#[derive(Debug)]
enum Heartbeat {
    Idle { next: Instant },
    Waiting { deadline: Instant, attempt: u8 },
}

#[derive(Debug)]
struct Connection {
    channel_id: u8,
    data_endpoint: SocketAddr,
    address: IndividualAddress,
    send_sequence: u8,
    recv_sequence: u8,
    heartbeat: Heartbeat,
    out_of_sequence_deadline: Option<Instant>,
    in_flight: Option<InFlight>,
}

#[derive(Debug)]
enum State {
    Connecting { deadline: Instant },
    Connected(Connection),
    WaitingToReconnect { at: Instant },
    Closing { channel_id: u8, deadline: Instant },
    Closed,
}

/// A KNXnet/IP tunnelling client state machine.
#[derive(Debug)]
pub struct TunnelClient {
    config: TunnelConfig,
    state: State,
    queue: VecDeque<Pending>,
    next_id: u64,
    backoff: Duration,
    transmits: VecDeque<Transmit>,
    events: VecDeque<Event>,
}

impl TunnelClient {
    /// Create a client and start connecting.
    pub fn new(mut config: TunnelConfig, now: Instant) -> Self {
        if config.transport == Transport::Tcp {
            config.local_endpoint = Hpai::TCP;
        }
        let backoff = config.reconnect.map_or(Duration::ZERO, |b| b.initial);
        let mut client = Self {
            config,
            state: State::Closed,
            queue: VecDeque::new(),
            next_id: 0,
            backoff,
            transmits: VecDeque::new(),
            events: VecDeque::new(),
        };
        client.start_connect(now);
        client
    }

    /// Report that the TCP stream closed (or could not be opened). Ignored over UDP.
    pub fn transport_closed(&mut self, now: Instant) {
        if self.config.transport != Transport::Tcp {
            return;
        }
        match self.state {
            State::Connected(_) | State::Connecting { .. } => {
                self.connection_lost(DisconnectReason::TransportClosed, now);
            }
            State::Closing { .. } => self.finish_close(),
            State::WaitingToReconnect { .. } | State::Closed => {}
        }
    }

    /// The configuration.
    pub fn config(&self) -> &TunnelConfig {
        &self.config
    }

    /// The tunnel's individual address while connected.
    pub fn address(&self) -> Option<IndividualAddress> {
        match &self.state {
            State::Connected(c) => Some(c.address),
            _ => None,
        }
    }

    /// Whether the tunnel is open.
    pub fn is_connected(&self) -> bool {
        matches!(self.state, State::Connected(_))
    }

    /// Whether the client has closed for good.
    pub fn is_closed(&self) -> bool {
        matches!(self.state, State::Closed)
    }

    /// Queue a cEMI frame (normally an `L_Data.req` with source `0.0.0`). Frames queued while
    /// disconnected are sent after the next successful connect.
    pub fn send(&mut self, cemi: &Cemi, now: Instant) -> Result<SendId, SendError> {
        if matches!(self.state, State::Closing { .. } | State::Closed) {
            return Err(SendError::Closed);
        }
        if self.queue.len() >= self.config.max_queue_len {
            return Err(SendError::QueueFull);
        }
        let bytes = cemi.to_vec().map_err(SendError::Encode)?;
        let id = SendId(self.next_id);
        self.next_id += 1;
        self.queue.push_back(Pending {
            id,
            cemi: bytes,
            expects_confirmation: matches!(cemi, Cemi::LDataReq(_)),
        });
        self.pump(now);
        Ok(id)
    }

    /// Disconnect and stop. Pending sends fail with [`SendError::Closed`]. [`Event::Closed`]
    /// follows once the server answers or the disconnect timeout passes.
    pub fn close(&mut self, now: Instant) {
        let state = std::mem::replace(&mut self.state, State::Closed);
        match state {
            State::Connected(conn) => {
                if let Some(in_flight) = conn.in_flight {
                    self.complete(in_flight.pending.id, Err(SendError::Closed));
                }
                self.fail_queue(SendError::Closed);
                self.transmit(
                    self.config.gateway,
                    &Frame::DisconnectRequest {
                        channel_id: conn.channel_id,
                        control_endpoint: self.config.local_endpoint,
                    },
                );
                self.state = State::Closing {
                    channel_id: conn.channel_id,
                    deadline: now + self.config.disconnect_timeout,
                };
            }
            State::Closing {
                channel_id,
                deadline,
            } => {
                self.state = State::Closing {
                    channel_id,
                    deadline,
                };
            }
            State::Closed => {}
            State::Connecting { .. } | State::WaitingToReconnect { .. } => self.finish_close(),
        }
    }

    /// Next datagram to send.
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.transmits.pop_front()
    }

    /// Next event.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// When [`TunnelClient::handle_timeout`] should next be called.
    pub fn poll_timeout(&self) -> Option<Instant> {
        match &self.state {
            State::Connecting { deadline } | State::Closing { deadline, .. } => Some(*deadline),
            State::WaitingToReconnect { at } => Some(*at),
            State::Closed => None,
            State::Connected(conn) => {
                let heartbeat = match conn.heartbeat {
                    Heartbeat::Idle { next } => next,
                    Heartbeat::Waiting { deadline, .. } => deadline,
                };
                let in_flight = conn.in_flight.as_ref().map(|f| match f.phase {
                    Phase::AwaitAck { deadline, .. } | Phase::AwaitConfirmation { deadline } => {
                        deadline
                    }
                });
                [Some(heartbeat), in_flight, conn.out_of_sequence_deadline]
                    .into_iter()
                    .flatten()
                    .min()
            }
        }
    }

    /// Process a datagram from `source`. Malformed and unexpected frames are ignored.
    pub fn handle_input(&mut self, source: SocketAddr, datagram: &[u8], now: Instant) {
        let frame = match Frame::decode(datagram) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::debug!(%source, %err, "ignoring undecodable datagram");
                return;
            }
        };
        match frame {
            Frame::ConnectResponse {
                channel_id,
                status,
                data_endpoint,
                crd,
            } => {
                if matches!(self.state, State::Connecting { .. }) {
                    self.on_connect_response(source, channel_id, status, data_endpoint, crd, now);
                }
            }
            Frame::ConnectionStateResponse { channel_id, status } => {
                self.on_heartbeat_response(channel_id, status, now);
            }
            Frame::DisconnectRequest {
                channel_id,
                control_endpoint,
            } => {
                self.on_disconnect_request(source, channel_id, control_endpoint, now);
            }
            Frame::DisconnectResponse { channel_id, .. } => {
                if matches!(self.state, State::Closing { channel_id: c, .. } if c == channel_id) {
                    self.finish_close();
                }
            }
            Frame::TunnellingRequest {
                channel_id,
                sequence,
                cemi,
            } => {
                self.on_tunnelling_request(channel_id, sequence, &cemi, now);
            }
            Frame::TunnellingAck {
                channel_id,
                sequence,
                status,
            } => {
                self.on_ack(channel_id, sequence, status, now);
            }
            other => tracing::trace!(service = other.service_type(), "ignoring frame"),
        }
        self.pump(now);
    }

    /// Drive timers. Call at or after the instant from [`TunnelClient::poll_timeout`].
    pub fn handle_timeout(&mut self, now: Instant) {
        match &mut self.state {
            State::Connecting { deadline } if now >= *deadline => {
                self.connection_lost(DisconnectReason::ConnectTimeout, now);
            }
            State::WaitingToReconnect { at } if now >= *at => self.start_connect(now),
            State::Closing { deadline, .. } if now >= *deadline => self.finish_close(),
            State::Connected(_) => self.connected_timeouts(now),
            _ => {}
        }
        self.pump(now);
    }

    fn connected_timeouts(&mut self, now: Instant) {
        let State::Connected(conn) = &mut self.state else {
            return;
        };
        let channel_id = conn.channel_id;
        let data_endpoint = conn.data_endpoint;

        if conn.out_of_sequence_deadline.is_some_and(|d| now >= d) {
            self.teardown(DisconnectReason::SequenceError, now);
            return;
        }

        // Heartbeat.
        let send_heartbeat = match conn.heartbeat {
            Heartbeat::Idle { next } if now >= next => {
                conn.heartbeat = Heartbeat::Waiting {
                    deadline: now + self.config.heartbeat_timeout,
                    attempt: 1,
                };
                true
            }
            Heartbeat::Waiting { deadline, attempt } if now >= deadline => {
                if attempt >= self.config.heartbeat_attempts {
                    self.teardown(DisconnectReason::HeartbeatFailed, now);
                    return;
                }
                conn.heartbeat = Heartbeat::Waiting {
                    deadline: now + self.config.heartbeat_timeout,
                    attempt: attempt + 1,
                };
                true
            }
            _ => false,
        };
        if send_heartbeat {
            self.send_heartbeat(channel_id);
        }

        // In-flight frame.
        let State::Connected(conn) = &mut self.state else {
            return;
        };
        let Some(in_flight) = &mut conn.in_flight else {
            return;
        };
        match in_flight.phase {
            Phase::AwaitAck { deadline, attempt } if now >= deadline => {
                if attempt >= 2 {
                    self.teardown(DisconnectReason::AckTimeout, now);
                    return;
                }
                in_flight.phase = Phase::AwaitAck {
                    deadline: now + self.config.ack_timeout,
                    attempt: attempt + 1,
                };
                let frame = Frame::TunnellingRequest {
                    channel_id,
                    sequence: in_flight.sequence,
                    cemi: in_flight.pending.cemi.clone(),
                };
                tracing::debug!(
                    sequence = in_flight.sequence,
                    "no ack, repeating tunnelling request"
                );
                self.transmit(data_endpoint, &frame);
            }
            Phase::AwaitConfirmation { deadline } if now >= deadline => {
                if let Some(in_flight) = conn.in_flight.take() {
                    self.complete(in_flight.pending.id, Err(SendError::ConfirmationTimeout));
                }
            }
            _ => {}
        }
    }

    fn start_connect(&mut self, now: Instant) {
        let frame = Frame::ConnectRequest {
            control_endpoint: self.config.local_endpoint,
            data_endpoint: self.config.local_endpoint,
            cri: Cri::Tunnel {
                layer: self.config.layer,
                address: self.config.requested_address,
            },
        };
        self.transmit(self.config.gateway, &frame);
        self.state = State::Connecting {
            deadline: now + self.config.connect_timeout,
        };
    }

    fn on_connect_response(
        &mut self,
        source: SocketAddr,
        channel_id: u8,
        status: Status,
        data_endpoint: Option<Hpai>,
        crd: Option<Crd>,
        now: Instant,
    ) {
        if !status.is_ok() {
            self.connection_lost(DisconnectReason::ConnectRejected(status), now);
            return;
        }
        let (Some(data_endpoint), Some(Crd::Tunnel { address })) = (data_endpoint, crd) else {
            self.connection_lost(DisconnectReason::InvalidConnectResponse, now);
            return;
        };
        self.backoff = self.config.reconnect.map_or(Duration::ZERO, |b| b.initial);
        self.state = State::Connected(Connection {
            channel_id,
            data_endpoint: data_endpoint.resolve(source),
            address,
            send_sequence: 0,
            recv_sequence: 0,
            heartbeat: Heartbeat::Idle {
                next: now + self.config.heartbeat_interval,
            },
            out_of_sequence_deadline: None,
            in_flight: None,
        });
        tracing::debug!(channel_id, %address, "tunnel connected");
        self.events.push_back(Event::Connected {
            address,
            channel_id,
        });
    }

    fn on_heartbeat_response(&mut self, channel_id: u8, status: Status, now: Instant) {
        let State::Connected(conn) = &mut self.state else {
            return;
        };
        if conn.channel_id != channel_id {
            return;
        }
        let Heartbeat::Waiting { attempt, .. } = conn.heartbeat else {
            return;
        };
        if status.is_ok() {
            conn.heartbeat = Heartbeat::Idle {
                next: now + self.config.heartbeat_interval,
            };
            return;
        }
        tracing::debug!(%status, attempt, "heartbeat refused");
        if attempt >= self.config.heartbeat_attempts {
            self.teardown(DisconnectReason::HeartbeatFailed, now);
        } else {
            conn.heartbeat = Heartbeat::Waiting {
                deadline: now + self.config.heartbeat_timeout,
                attempt: attempt + 1,
            };
            self.send_heartbeat(channel_id);
        }
    }

    fn on_disconnect_request(
        &mut self,
        source: SocketAddr,
        channel_id: u8,
        endpoint: Hpai,
        now: Instant,
    ) {
        let ours = match &self.state {
            State::Connected(conn) => conn.channel_id == channel_id,
            State::Closing { channel_id: c, .. } => *c == channel_id,
            _ => false,
        };
        if !ours {
            return;
        }
        self.transmit(
            endpoint.resolve(source),
            &Frame::DisconnectResponse {
                channel_id,
                status: Status::NO_ERROR,
            },
        );
        if matches!(self.state, State::Closing { .. }) {
            self.finish_close();
        } else {
            self.connection_lost(DisconnectReason::ServerDisconnected, now);
        }
    }

    fn on_tunnelling_request(&mut self, channel_id: u8, sequence: u8, cemi: &[u8], now: Instant) {
        let State::Connected(conn) = &mut self.state else {
            return;
        };
        if conn.channel_id != channel_id {
            return;
        }
        if self.config.transport == Transport::Tcp {
            // No acks over TCP, and the stream keeps frames in order.
            self.on_cemi(cemi, now);
            return;
        }
        let ack = Frame::TunnellingAck {
            channel_id,
            sequence,
            status: Status::NO_ERROR,
        };
        let data_endpoint = conn.data_endpoint;
        if sequence == conn.recv_sequence {
            conn.recv_sequence = conn.recv_sequence.wrapping_add(1);
            conn.out_of_sequence_deadline = None;
            self.transmit(data_endpoint, &ack);
            self.on_cemi(cemi, now);
        } else if sequence == conn.recv_sequence.wrapping_sub(1) {
            tracing::debug!(sequence, "repeated tunnelling request, acknowledging again");
            self.transmit(data_endpoint, &ack);
        } else {
            tracing::debug!(
                sequence,
                expected = conn.recv_sequence,
                "out-of-sequence tunnelling request dropped"
            );
            conn.out_of_sequence_deadline
                .get_or_insert(now + self.config.out_of_sequence_timeout);
        }
    }

    fn on_cemi(&mut self, bytes: &[u8], now: Instant) {
        match Cemi::decode(bytes) {
            Ok(Cemi::LDataCon(ldata)) => self.on_confirmation(ldata.confirm_error, now),
            Ok(cemi) => self.events.push_back(Event::Received(cemi)),
            Err(err) => tracing::debug!(%err, "ignoring undecodable cEMI frame"),
        }
    }

    fn on_confirmation(&mut self, error: bool, _now: Instant) {
        let State::Connected(conn) = &mut self.state else {
            return;
        };
        let Some(in_flight) = &mut conn.in_flight else {
            tracing::debug!("L_Data.con with nothing outstanding");
            return;
        };
        if !in_flight.pending.expects_confirmation {
            return;
        }
        match in_flight.phase {
            Phase::AwaitAck { .. } => in_flight.early_confirmation = Some(error),
            Phase::AwaitConfirmation { .. } => {
                if let Some(done) = conn.in_flight.take() {
                    let result = if error {
                        Err(SendError::NegativeConfirmation)
                    } else {
                        Ok(())
                    };
                    self.complete(done.pending.id, result);
                }
            }
        }
    }

    fn on_ack(&mut self, channel_id: u8, sequence: u8, status: Status, now: Instant) {
        let State::Connected(conn) = &mut self.state else {
            return;
        };
        if conn.channel_id != channel_id {
            return;
        }
        let Some(in_flight) = &mut conn.in_flight else {
            return;
        };
        let Phase::AwaitAck { attempt, .. } = in_flight.phase else {
            return;
        };
        if in_flight.sequence != sequence {
            tracing::debug!(
                sequence,
                expected = in_flight.sequence,
                "ack for another sequence number ignored"
            );
            return;
        }
        if !status.is_ok() {
            tracing::debug!(%status, "tunnelling request refused");
            if attempt >= 2 {
                self.teardown(DisconnectReason::AckTimeout, now);
            } else {
                in_flight.phase = Phase::AwaitAck {
                    deadline: now + self.config.ack_timeout,
                    attempt: attempt + 1,
                };
                let frame = Frame::TunnellingRequest {
                    channel_id,
                    sequence,
                    cemi: in_flight.pending.cemi.clone(),
                };
                let data_endpoint = conn.data_endpoint;
                self.transmit(data_endpoint, &frame);
            }
            return;
        }
        conn.send_sequence = conn.send_sequence.wrapping_add(1);
        let done = match (
            in_flight.pending.expects_confirmation,
            in_flight.early_confirmation,
        ) {
            (false, _) => Some(Ok(())),
            (true, Some(false)) => Some(Ok(())),
            (true, Some(true)) => Some(Err(SendError::NegativeConfirmation)),
            (true, None) => {
                in_flight.phase = Phase::AwaitConfirmation {
                    deadline: now + self.config.confirmation_timeout,
                };
                None
            }
        };
        if let Some(result) = done
            && let Some(in_flight) = conn.in_flight.take()
        {
            self.complete(in_flight.pending.id, result);
        }
    }

    /// Send the next queued frame if the tunnel is idle.
    fn pump(&mut self, now: Instant) {
        let State::Connected(conn) = &mut self.state else {
            return;
        };
        if conn.in_flight.is_some() {
            return;
        }
        let Some(pending) = self.queue.pop_front() else {
            return;
        };
        let frame = Frame::TunnellingRequest {
            channel_id: conn.channel_id,
            sequence: conn.send_sequence,
            cemi: pending.cemi.clone(),
        };
        let data_endpoint = conn.data_endpoint;
        let sequence = conn.send_sequence;
        if self.config.transport == Transport::Tcp {
            // No ack to wait for: go straight to the confirmation, if any.
            conn.send_sequence = conn.send_sequence.wrapping_add(1);
            if pending.expects_confirmation {
                conn.in_flight = Some(InFlight {
                    pending,
                    sequence,
                    phase: Phase::AwaitConfirmation {
                        deadline: now + self.config.confirmation_timeout,
                    },
                    early_confirmation: None,
                });
                self.transmit(data_endpoint, &frame);
            } else {
                let id = pending.id;
                self.transmit(data_endpoint, &frame);
                self.complete(id, Ok(()));
                self.pump(now);
            }
            return;
        }
        conn.in_flight = Some(InFlight {
            pending,
            sequence,
            phase: Phase::AwaitAck {
                deadline: now + self.config.ack_timeout,
                attempt: 1,
            },
            early_confirmation: None,
        });
        self.transmit(data_endpoint, &frame);
    }

    /// Tear down an open connection we decided is broken: tell the server, then reconnect.
    fn teardown(&mut self, reason: DisconnectReason, now: Instant) {
        if let State::Connected(conn) = &self.state {
            let frame = Frame::DisconnectRequest {
                channel_id: conn.channel_id,
                control_endpoint: self.config.local_endpoint,
            };
            self.transmit(self.config.gateway, &frame);
        }
        self.connection_lost(reason, now);
    }

    fn connection_lost(&mut self, reason: DisconnectReason, now: Instant) {
        let state = std::mem::replace(&mut self.state, State::Closed);
        if let State::Connected(conn) = state
            && let Some(in_flight) = conn.in_flight
        {
            let error = if reason == DisconnectReason::AckTimeout {
                SendError::NotAcknowledged
            } else {
                SendError::ConnectionLost
            };
            self.complete(in_flight.pending.id, Err(error));
        }
        tracing::debug!(%reason, "tunnel disconnected");
        match self.config.reconnect {
            Some(backoff) => {
                let at = now + self.backoff;
                self.backoff = (self.backoff * 2).min(backoff.max);
                self.state = State::WaitingToReconnect { at };
                self.events.push_back(Event::Disconnected {
                    reason,
                    reconnect_at: Some(at),
                });
            }
            None => {
                self.events.push_back(Event::Disconnected {
                    reason,
                    reconnect_at: None,
                });
                self.finish_close();
            }
        }
    }

    fn finish_close(&mut self) {
        self.state = State::Closed;
        self.fail_queue(SendError::Closed);
        self.events.push_back(Event::Closed);
    }

    fn fail_queue(&mut self, error: SendError) {
        while let Some(pending) = self.queue.pop_front() {
            self.complete(pending.id, Err(error));
        }
    }

    fn send_heartbeat(&mut self, channel_id: u8) {
        let frame = Frame::ConnectionStateRequest {
            channel_id,
            control_endpoint: self.config.local_endpoint,
        };
        self.transmit(self.config.gateway, &frame);
    }

    fn complete(&mut self, id: SendId, result: Result<(), SendError>) {
        self.events.push_back(Event::SendResult { id, result });
    }

    fn transmit(&mut self, destination: SocketAddr, frame: &Frame) {
        match frame.to_vec() {
            Ok(payload) => self.transmits.push_back(Transmit {
                destination,
                payload,
            }),
            // Only frames this module builds reach here, and they always encode.
            Err(err) => tracing::error!(%err, "could not encode KNXnet/IP frame"),
        }
    }
}
