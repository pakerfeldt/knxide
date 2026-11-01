//! Sans-IO KNXnet/IP tunnelling server: the gateway side of a tunnel. Spec: 3/8/4
//! (Tunnelling), 3/8/2 (Core) §4 and §5.
//!
//! Same shape as [`crate::tunnel::TunnelClient`]: feed datagrams and time in, drain transmits,
//! events and the next deadline. The server answers search and description requests,
//! accepts link-layer tunnels (each client gets an individual address from a pool),
//! acknowledges and sequences frames in both directions, repeats unacknowledged frames once
//! and drops clients that stop sending heartbeats. What to do with the frames is up to the
//! caller (a proxy forwards them to a real bus).
//!
//! UDP only for now.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use knxide_core::{Cemi, IndividualAddress};

use crate::frame::{Frame, Status};
use crate::structures::{Crd, Cri, DeviceInfo, Dib, Hpai, Medium, ServiceFamily, TunnelLayer};
use crate::tunnel::Transmit;

/// Server settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// Our control (and data) endpoint as clients should reach it. Sent in search,
    /// description and connect responses.
    pub endpoint: SocketAddrV4,
    /// Friendly name in the device information DIB (at most 30 characters).
    pub name: String,
    /// The server's own individual address, shown in the device information DIB.
    pub address: IndividualAddress,
    /// Addresses handed to tunnel clients, one each. Its length is the connection limit.
    pub client_addresses: Vec<IndividualAddress>,
    /// KNX serial number in the device information DIB.
    pub serial_number: [u8; 6],
    /// MAC address in the device information DIB.
    pub mac_address: [u8; 6],
    /// Drop a client that sends no `CONNECTIONSTATE_REQUEST` for this long: 120 s
    /// (3/8/2 §5.4).
    pub heartbeat_timeout: Duration,
    /// `TUNNELLING_REQUEST_TIMEOUT`: 1 s.
    pub ack_timeout: Duration,
    /// Most frames waiting for one client.
    pub max_queue_len: usize,
}

impl ServerConfig {
    /// Defaults for a server reachable at `endpoint`, with client addresses
    /// `15.15.241`–`15.15.254`.
    pub fn new(endpoint: SocketAddrV4) -> Self {
        Self {
            endpoint,
            name: "knxide".to_string(),
            address: IndividualAddress::from_raw(0xFFF0),
            client_addresses: (241..=254)
                .map(|d| IndividualAddress::from_raw(0xFF00 | d))
                .collect(),
            serial_number: [0x00, 0x00, 0x6B, 0x6E, 0x78, 0x00],
            mac_address: [0x02, 0x00, 0x6B, 0x6E, 0x78, 0x00],
            heartbeat_timeout: Duration::from_secs(120),
            ack_timeout: Duration::from_secs(1),
            max_queue_len: 256,
        }
    }
}

/// Why a client went away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientGone {
    /// It sent `DISCONNECT_REQUEST`.
    Disconnected,
    /// No heartbeat within the timeout.
    HeartbeatTimeout,
    /// It did not acknowledge a frame twice.
    AckTimeout,
    /// The server disconnected it.
    Closed,
}

/// Something that happened, drained with [`TunnelServer::poll_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    /// A client opened a tunnel.
    Connected {
        /// Channel id, which identifies the client from now on.
        channel: u8,
        /// The individual address assigned to it.
        address: IndividualAddress,
        /// Where its frames go.
        endpoint: SocketAddr,
    },
    /// A client's tunnel closed.
    Disconnected {
        /// Channel id.
        channel: u8,
        /// The address it had.
        address: IndividualAddress,
        /// Why.
        reason: ClientGone,
    },
    /// A client sent a cEMI frame (normally `L_Data.req`). Already acknowledged.
    Frame {
        /// Channel id.
        channel: u8,
        /// The frame.
        cemi: Cemi,
    },
}

/// A frame could not be queued for a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerSendError {
    /// No such channel.
    UnknownChannel,
    /// The client's queue is full.
    QueueFull,
}

#[derive(Debug)]
struct InFlight {
    sequence: u8,
    payload: Vec<u8>,
    deadline: Instant,
    attempt: u8,
}

#[derive(Debug)]
struct Client {
    channel: u8,
    address: IndividualAddress,
    control: SocketAddr,
    data: SocketAddr,
    recv_sequence: u8,
    send_sequence: u8,
    heartbeat_deadline: Instant,
    outbox: VecDeque<Vec<u8>>,
    in_flight: Option<InFlight>,
}

/// A KNXnet/IP tunnelling server state machine.
#[derive(Debug)]
pub struct TunnelServer {
    config: ServerConfig,
    clients: Vec<Client>,
    next_channel: u8,
    bus_connected: bool,
    transmits: VecDeque<Transmit>,
    events: VecDeque<ServerEvent>,
}

impl TunnelServer {
    /// A server with no clients. The bus is assumed connected.
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            clients: Vec::new(),
            next_channel: 1,
            bus_connected: true,
            transmits: VecDeque::new(),
            events: VecDeque::new(),
        }
    }

    /// The configuration.
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Connected clients: channel and address.
    pub fn clients(&self) -> impl Iterator<Item = (u8, IndividualAddress)> + '_ {
        self.clients.iter().map(|c| (c.channel, c.address))
    }

    /// The address assigned to `channel`.
    pub fn client_address(&self, channel: u8) -> Option<IndividualAddress> {
        self.clients
            .iter()
            .find(|c| c.channel == channel)
            .map(|c| c.address)
    }

    /// Whether the KNX side is reachable. While not, heartbeats are answered with
    /// `E_KNX_CONNECTION` and connect requests are refused with it.
    pub fn set_bus_connected(&mut self, connected: bool) {
        self.bus_connected = connected;
    }

    /// Next datagram to send.
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.transmits.pop_front()
    }

    /// Next event.
    pub fn poll_event(&mut self) -> Option<ServerEvent> {
        self.events.pop_front()
    }

    /// When [`TunnelServer::handle_timeout`] should next be called.
    pub fn poll_timeout(&self) -> Option<Instant> {
        self.clients
            .iter()
            .flat_map(|c| {
                [
                    Some(c.heartbeat_deadline),
                    c.in_flight.as_ref().map(|f| f.deadline),
                ]
            })
            .flatten()
            .min()
    }

    /// Queue a frame for one client.
    pub fn send(&mut self, channel: u8, cemi: &Cemi, now: Instant) -> Result<(), ServerSendError> {
        let Ok(payload) = cemi.to_vec() else {
            return Ok(());
        };
        let max = self.config.max_queue_len;
        let client = self
            .clients
            .iter_mut()
            .find(|c| c.channel == channel)
            .ok_or(ServerSendError::UnknownChannel)?;
        if client.outbox.len() >= max {
            return Err(ServerSendError::QueueFull);
        }
        client.outbox.push_back(payload);
        self.pump(now);
        Ok(())
    }

    /// Queue a frame for every client except `except`. Clients with a full queue miss it.
    pub fn send_all(&mut self, cemi: &Cemi, except: Option<u8>, now: Instant) {
        let channels: Vec<u8> = self
            .clients
            .iter()
            .map(|c| c.channel)
            .filter(|&c| Some(c) != except)
            .collect();
        for channel in channels {
            if self.send(channel, cemi, now) == Err(ServerSendError::QueueFull) {
                tracing::warn!(channel, "client queue full, frame dropped");
            }
        }
    }

    /// Disconnect every client (e.g. when shutting down).
    pub fn disconnect_all(&mut self) {
        for client in std::mem::take(&mut self.clients) {
            self.transmit(
                client.control,
                &Frame::DisconnectRequest {
                    channel_id: client.channel,
                    control_endpoint: Hpai::udp(self.config.endpoint),
                },
            );
            self.events.push_back(ServerEvent::Disconnected {
                channel: client.channel,
                address: client.address,
                reason: ClientGone::Closed,
            });
        }
    }

    /// Process a datagram.
    pub fn handle_input(&mut self, source: SocketAddr, datagram: &[u8], now: Instant) {
        let frame = match Frame::decode(datagram) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::debug!(%source, %err, "ignoring undecodable datagram");
                return;
            }
        };
        match frame {
            Frame::SearchRequest { discovery_endpoint } => {
                let response = Frame::SearchResponse {
                    control_endpoint: Hpai::udp(self.config.endpoint),
                    dibs: self.dibs(),
                };
                self.transmit(discovery_endpoint.resolve(source), &response);
            }
            Frame::DescriptionRequest { control_endpoint } => {
                self.transmit(
                    control_endpoint.resolve(source),
                    &Frame::DescriptionResponse { dibs: self.dibs() },
                );
            }
            Frame::ConnectRequest {
                control_endpoint,
                data_endpoint,
                cri,
            } => {
                self.on_connect(source, control_endpoint, data_endpoint, cri, now);
            }
            Frame::ConnectionStateRequest {
                channel_id,
                control_endpoint,
            } => {
                let reply_to = control_endpoint.resolve(source);
                let status = match self.clients.iter_mut().find(|c| c.channel == channel_id) {
                    None => Status::CONNECTION_ID,
                    Some(client) => {
                        client.heartbeat_deadline = now + self.config.heartbeat_timeout;
                        if self.bus_connected {
                            Status::NO_ERROR
                        } else {
                            Status::KNX_CONNECTION
                        }
                    }
                };
                self.transmit(
                    reply_to,
                    &Frame::ConnectionStateResponse { channel_id, status },
                );
            }
            Frame::DisconnectRequest {
                channel_id,
                control_endpoint,
            } => {
                let reply_to = control_endpoint.resolve(source);
                let status = if self.remove(channel_id, ClientGone::Disconnected) {
                    Status::NO_ERROR
                } else {
                    Status::CONNECTION_ID
                };
                self.transmit(reply_to, &Frame::DisconnectResponse { channel_id, status });
            }
            Frame::TunnellingRequest {
                channel_id,
                sequence,
                cemi,
            } => {
                self.on_tunnelling_request(channel_id, sequence, &cemi);
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
    }

    /// Drive timers: client heartbeats and frame acknowledgements.
    pub fn handle_timeout(&mut self, now: Instant) {
        let expired: Vec<u8> = self
            .clients
            .iter()
            .filter(|c| now >= c.heartbeat_deadline)
            .map(|c| c.channel)
            .collect();
        for channel in expired {
            tracing::debug!(channel, "client heartbeat timed out");
            self.disconnect(channel, ClientGone::HeartbeatTimeout);
        }

        let mut gone = Vec::new();
        for client in &mut self.clients {
            let Some(in_flight) = &mut client.in_flight else {
                continue;
            };
            if now < in_flight.deadline {
                continue;
            }
            if in_flight.attempt >= 2 {
                gone.push(client.channel);
                continue;
            }
            in_flight.attempt += 1;
            in_flight.deadline = now + self.config.ack_timeout;
            let frame = Frame::TunnellingRequest {
                channel_id: client.channel,
                sequence: in_flight.sequence,
                cemi: in_flight.payload.clone(),
            };
            if let Ok(payload) = frame.to_vec() {
                self.transmits.push_back(Transmit {
                    destination: client.data,
                    payload,
                });
            }
        }
        for channel in gone {
            tracing::debug!(channel, "client did not acknowledge, dropping it");
            self.disconnect(channel, ClientGone::AckTimeout);
        }
        self.pump(now);
    }

    fn dibs(&self) -> Vec<Dib> {
        vec![
            Dib::DeviceInfo(DeviceInfo {
                medium: Medium::TP1,
                status: 0,
                address: self.config.address,
                project_installation_id: 0,
                serial_number: self.config.serial_number,
                multicast_address: Ipv4Addr::UNSPECIFIED,
                mac_address: self.config.mac_address,
                name: self.config.name.chars().take(30).collect(),
            }),
            Dib::SupportedServiceFamilies(vec![
                ServiceFamily {
                    id: ServiceFamily::CORE,
                    version: 1,
                },
                ServiceFamily {
                    id: ServiceFamily::TUNNELLING,
                    version: 1,
                },
            ]),
        ]
    }

    fn on_connect(
        &mut self,
        source: SocketAddr,
        control: Hpai,
        data: Hpai,
        cri: Cri,
        now: Instant,
    ) {
        let reply_to = control.resolve(source);
        let refuse = |status| Frame::ConnectResponse {
            channel_id: 0,
            status,
            data_endpoint: None,
            crd: None,
        };
        let requested = match cri {
            Cri::Tunnel {
                layer: TunnelLayer::LinkLayer,
                address,
            } => address,
            Cri::Tunnel { .. } => {
                self.transmit(reply_to, &refuse(Status::TUNNELLING_LAYER));
                return;
            }
            Cri::Other { .. } => {
                self.transmit(reply_to, &refuse(Status::CONNECTION_TYPE));
                return;
            }
        };
        if !self.bus_connected {
            self.transmit(reply_to, &refuse(Status::KNX_CONNECTION));
            return;
        }
        let in_use = |a: IndividualAddress| self.clients.iter().any(|c| c.address == a);
        let address = match requested {
            Some(a) if !self.config.client_addresses.contains(&a) => {
                self.transmit(reply_to, &refuse(Status::NO_TUNNELLING_ADDRESS));
                return;
            }
            Some(a) if in_use(a) => {
                self.transmit(reply_to, &refuse(Status::CONNECTION_IN_USE));
                return;
            }
            Some(a) => a,
            None => match self
                .config
                .client_addresses
                .iter()
                .copied()
                .find(|&a| !in_use(a))
            {
                Some(a) => a,
                None => {
                    self.transmit(reply_to, &refuse(Status::NO_MORE_CONNECTIONS));
                    return;
                }
            },
        };
        let channel = self.allocate_channel();
        // A client in NAT mode gets route-back answers: it then talks to whatever address
        // reached it, which works whether it came in over loopback or the LAN.
        let data_endpoint = if data.is_route_back() {
            Hpai::ROUTE_BACK
        } else {
            Hpai::udp(self.config.endpoint)
        };
        let client = Client {
            channel,
            address,
            control: reply_to,
            data: data.resolve(source),
            recv_sequence: 0,
            send_sequence: 0,
            heartbeat_deadline: now + self.config.heartbeat_timeout,
            outbox: VecDeque::new(),
            in_flight: None,
        };
        let endpoint = client.data;
        self.clients.push(client);
        self.transmit(
            reply_to,
            &Frame::ConnectResponse {
                channel_id: channel,
                status: Status::NO_ERROR,
                data_endpoint: Some(data_endpoint),
                crd: Some(Crd::Tunnel { address }),
            },
        );
        tracing::debug!(channel, %address, %endpoint, "client connected");
        self.events.push_back(ServerEvent::Connected {
            channel,
            address,
            endpoint,
        });
    }

    fn allocate_channel(&mut self) -> u8 {
        loop {
            let channel = self.next_channel;
            self.next_channel = self.next_channel.wrapping_add(1).max(1);
            if !self.clients.iter().any(|c| c.channel == channel) {
                return channel;
            }
        }
    }

    fn on_tunnelling_request(&mut self, channel: u8, sequence: u8, cemi: &[u8]) {
        let Some(client) = self.clients.iter_mut().find(|c| c.channel == channel) else {
            return;
        };
        let ack = Frame::TunnellingAck {
            channel_id: channel,
            sequence,
            status: Status::NO_ERROR,
        };
        let data = client.data;
        if sequence == client.recv_sequence {
            client.recv_sequence = client.recv_sequence.wrapping_add(1);
            self.transmit(data, &ack);
            match Cemi::decode(cemi) {
                Ok(cemi) => self.events.push_back(ServerEvent::Frame { channel, cemi }),
                Err(err) => tracing::debug!(channel, %err, "ignoring undecodable cEMI frame"),
            }
        } else if sequence == client.recv_sequence.wrapping_sub(1) {
            self.transmit(data, &ack);
        } else {
            tracing::debug!(
                channel,
                sequence,
                expected = client.recv_sequence,
                "out-of-sequence frame dropped"
            );
        }
    }

    fn on_ack(&mut self, channel: u8, sequence: u8, status: Status, now: Instant) {
        let Some(client) = self.clients.iter_mut().find(|c| c.channel == channel) else {
            return;
        };
        let Some(in_flight) = &client.in_flight else {
            return;
        };
        if in_flight.sequence != sequence {
            return;
        }
        if !status.is_ok() {
            // Treated like a missing ack: the timer repeats it once, then drops the client.
            return;
        }
        client.in_flight = None;
        client.send_sequence = client.send_sequence.wrapping_add(1);
        self.pump(now);
    }

    /// Send the next queued frame to every idle client.
    fn pump(&mut self, now: Instant) {
        let ack_timeout = self.config.ack_timeout;
        for client in &mut self.clients {
            if client.in_flight.is_some() {
                continue;
            }
            let Some(payload) = client.outbox.pop_front() else {
                continue;
            };
            let frame = Frame::TunnellingRequest {
                channel_id: client.channel,
                sequence: client.send_sequence,
                cemi: payload.clone(),
            };
            if let Ok(bytes) = frame.to_vec() {
                self.transmits.push_back(Transmit {
                    destination: client.data,
                    payload: bytes,
                });
            }
            client.in_flight = Some(InFlight {
                sequence: client.send_sequence,
                payload,
                deadline: now + ack_timeout,
                attempt: 1,
            });
        }
    }

    /// Drop a client and tell it.
    fn disconnect(&mut self, channel: u8, reason: ClientGone) {
        if let Some(client) = self.clients.iter().find(|c| c.channel == channel) {
            let frame = Frame::DisconnectRequest {
                channel_id: channel,
                control_endpoint: Hpai::udp(self.config.endpoint),
            };
            let control = client.control;
            self.transmit(control, &frame);
        }
        self.remove(channel, reason);
    }

    fn remove(&mut self, channel: u8, reason: ClientGone) -> bool {
        let Some(index) = self.clients.iter().position(|c| c.channel == channel) else {
            return false;
        };
        let client = self.clients.swap_remove(index);
        tracing::debug!(channel, address = %client.address, ?reason, "client gone");
        self.events.push_back(ServerEvent::Disconnected {
            channel,
            address: client.address,
            reason,
        });
        true
    }

    fn transmit(&mut self, destination: SocketAddr, frame: &Frame) {
        match frame.to_vec() {
            Ok(payload) => self.transmits.push_back(Transmit {
                destination,
                payload,
            }),
            Err(err) => tracing::error!(%err, "could not encode KNXnet/IP frame"),
        }
    }
}
