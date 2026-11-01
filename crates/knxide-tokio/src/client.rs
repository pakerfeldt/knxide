//! The async tunnelling client: a cloneable handle in front of one driver task that owns the
//! UDP socket and the `knxide-ip` state machine.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use knxide_core::dpt::{DptId, DptValue};
use knxide_core::{Apdu, Cemi, GroupAddress, IndividualAddress, LData, Payload};
use knxide_ip::Hpai;
use knxide_ip::tunnel::{
    DisconnectReason, Event, SendError, SendId, Transport, TunnelClient, TunnelConfig,
};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::error::Error;
use crate::link::{Link, Received};

/// Options for [`Client::connect`].
#[derive(Debug, Clone)]
pub struct TunnelOptions {
    /// Local address to bind (UDP). Default `0.0.0.0:0`.
    pub bind: SocketAddr,
    /// Route-back (NAT) mode: advertise `0.0.0.0:0` and let the server reply to the source
    /// address. Default on; it works through NAT and container networks. When off, the local
    /// address used to reach the gateway is advertised.
    pub nat: bool,
    /// Report our own successfully sent `L_Data.req` frames as [`ClientEvent::Frame`], as if
    /// they had been received from the bus. The server does not echo them.
    pub local_echo: bool,
    /// Capacity of the event broadcast. Slow subscribers miss events beyond this.
    pub event_capacity: usize,
    /// Protocol settings: layer, timers, reconnect back-off. `gateway` and `local_endpoint`
    /// are set by [`Client::connect`].
    pub tunnel: TunnelConfig,
}

impl Default for TunnelOptions {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            nat: true,
            local_echo: false,
            event_capacity: 256,
            tunnel: TunnelConfig::new(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))),
        }
    }
}

/// Something that happened on the connection or the bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// The tunnel is open (again).
    Connected {
        /// The tunnel's individual address.
        address: IndividualAddress,
    },
    /// The tunnel was lost. The client reconnects on its own unless reconnect is disabled.
    Disconnected {
        /// Why.
        reason: DisconnectReason,
    },
    /// An `L_Data` frame from the bus (or our own, with [`TunnelOptions::local_echo`]).
    Frame(LData),
    /// Any other cEMI message.
    Other(Cemi),
    /// The client closed.
    Closed,
}

/// Connection state, as seen by [`Client::state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Connecting or waiting to reconnect.
    Connecting,
    /// Open, with this tunnel address.
    Connected(IndividualAddress),
    /// Closed for good.
    Closed,
}

pub(crate) enum Command {
    Send {
        cemi: Cemi,
        reply: oneshot::Sender<Result<(), SendError>>,
    },
    Close,
}

/// A KNX client over a tunnel ([`Client::connect`]) or routing ([`Client::routing`]). Cheap to
/// clone; all clones share one connection, which closes when the last clone is dropped or
/// [`Client::close`] is called.
#[derive(Clone)]
pub struct Client {
    pub(crate) commands: mpsc::Sender<Command>,
    pub(crate) events: broadcast::Sender<ClientEvent>,
    pub(crate) state: watch::Receiver<ConnectionState>,
}

impl Client {
    /// Connect to the tunnelling server at `gateway`. Resolves once the tunnel is open; fails
    /// if the first attempt is refused or times out. Later losses reconnect automatically.
    pub async fn connect(gateway: SocketAddr, options: TunnelOptions) -> Result<Self, Error> {
        let mut config = options.tunnel.clone();
        config.gateway = gateway;
        let link = if config.transport == Transport::Tcp {
            Link::tcp(gateway, config.connect_timeout)
        } else {
            let socket = UdpSocket::bind(options.bind).await?;
            config.local_endpoint = if options.nat {
                Hpai::ROUTE_BACK
            } else {
                Hpai::udp(local_endpoint_towards(&socket, gateway).await?)
            };
            Link::udp(socket)
        };

        let (commands, command_rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(options.event_capacity.max(1));
        let (state_tx, state) = watch::channel(ConnectionState::Connecting);
        let (first_tx, first_rx) = oneshot::channel();
        let driver = Driver {
            tunnel: TunnelClient::new(config, Instant::now()),
            link,
            commands: command_rx,
            commands_open: true,
            events: events.clone(),
            state: state_tx,
            local_echo: options.local_echo,
            pending: HashMap::new(),
            first_connect: Some(first_tx),
        };
        tokio::spawn(driver.run());
        match first_rx.await {
            Ok(Ok(())) => Ok(Self {
                commands,
                events,
                state,
            }),
            Ok(Err(reason)) => Err(Error::Connect(reason)),
            Err(_) => Err(Error::Closed),
        }
    }

    /// Current connection state.
    pub fn state(&self) -> ConnectionState {
        *self.state.borrow()
    }

    /// The tunnel's individual address while connected.
    pub fn address(&self) -> Option<IndividualAddress> {
        match self.state() {
            ConnectionState::Connected(address) => Some(address),
            _ => None,
        }
    }

    /// Wait until the tunnel is open (returns at once if it is).
    pub async fn wait_connected(&self) -> Result<IndividualAddress, Error> {
        let mut state = self.state.clone();
        let result = state
            .wait_for(|s| !matches!(s, ConnectionState::Connecting))
            .await
            .map_err(|_| Error::Closed)?;
        match *result {
            ConnectionState::Connected(address) => Ok(address),
            _ => Err(Error::Closed),
        }
    }

    /// Receive connection events and bus traffic from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<ClientEvent> {
        self.events.subscribe()
    }

    /// Send a cEMI frame. For `L_Data.req`, resolves when the interface confirms it.
    pub async fn send(&self, cemi: Cemi) -> Result<(), Error> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Send { cemi, reply })
            .await
            .map_err(|_| Error::Closed)?;
        result
            .await
            .map_err(|_| Error::Closed)?
            .map_err(Error::Send)
    }

    /// `GroupValue_Write`. Resolves when the interface confirms the frame on the bus.
    pub async fn group_write(&self, ga: GroupAddress, payload: Payload) -> Result<(), Error> {
        self.send(Cemi::LDataReq(LData::group(
            ga,
            Apdu::GroupValueWrite(payload),
        )))
        .await
    }

    /// `GroupValue_Write` of a typed value.
    pub async fn group_write_value(
        &self,
        ga: GroupAddress,
        dpt: DptId,
        value: &DptValue,
    ) -> Result<(), Error> {
        self.group_write(ga, dpt.encode(value)?).await
    }

    /// `GroupValue_Response`, for applications that answer reads.
    pub async fn group_respond(&self, ga: GroupAddress, payload: Payload) -> Result<(), Error> {
        self.send(Cemi::LDataReq(LData::group(
            ga,
            Apdu::GroupValueResponse(payload),
        )))
        .await
    }

    /// `GroupValue_Read`: ask the bus for the value of `ga` and wait for the first
    /// `GroupValue_Response` within `timeout`.
    pub async fn group_read(&self, ga: GroupAddress, timeout: Duration) -> Result<Payload, Error> {
        let (_, payload) = self.group_read_from(ga, timeout).await?;
        Ok(payload)
    }

    /// Like [`Client::group_read`], also returning which device answered.
    pub async fn group_read_from(
        &self,
        ga: GroupAddress,
        timeout: Duration,
    ) -> Result<(IndividualAddress, Payload), Error> {
        let mut events = self.subscribe();
        let read = async {
            self.send(Cemi::LDataReq(LData::group(ga, Apdu::GroupValueRead)))
                .await?;
            loop {
                match events.recv().await {
                    Ok(ClientEvent::Frame(ldata)) if ldata.group_address() == Some(ga) => {
                        if let Some(Apdu::GroupValueResponse(payload)) = ldata.apdu() {
                            return Ok((ldata.source, payload.clone()));
                        }
                    }
                    Ok(ClientEvent::Closed) | Err(broadcast::error::RecvError::Closed) => {
                        return Err(Error::Closed);
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        };
        tokio::time::timeout(timeout, read)
            .await
            .map_err(|_| Error::Timeout)?
    }

    /// `GroupValue_Read` decoded as `dpt`.
    pub async fn group_read_value(
        &self,
        ga: GroupAddress,
        dpt: DptId,
        timeout: Duration,
    ) -> Result<DptValue, Error> {
        Ok(dpt.decode(&self.group_read(ga, timeout).await?)?)
    }

    /// Disconnect and wait until the client has closed. Other clones stop working too.
    pub async fn close(self) {
        let mut state = self.state.clone();
        let _ = self.commands.send(Command::Close).await;
        let _ = state.wait_for(|s| *s == ConnectionState::Closed).await;
    }
}

/// The local IPv4 address the OS would use to reach `gateway`, with our socket's port.
async fn local_endpoint_towards(
    socket: &UdpSocket,
    gateway: SocketAddr,
) -> Result<SocketAddrV4, Error> {
    let probe = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
    probe.connect(gateway).await?;
    let (SocketAddr::V4(local), SocketAddr::V4(bound)) =
        (probe.local_addr()?, socket.local_addr()?)
    else {
        return Err(Error::Unsupported("IPv6"));
    };
    Ok(SocketAddrV4::new(*local.ip(), bound.port()))
}

/// Reply channel of a send, and the frame to echo locally once it succeeds.
pub(crate) type PendingSend = (oneshot::Sender<Result<(), SendError>>, Option<LData>);

struct Driver {
    tunnel: TunnelClient,
    link: Link,
    commands: mpsc::Receiver<Command>,
    commands_open: bool,
    events: broadcast::Sender<ClientEvent>,
    state: watch::Sender<ConnectionState>,
    local_echo: bool,
    pending: HashMap<SendId, PendingSend>,
    first_connect: Option<oneshot::Sender<Result<(), DisconnectReason>>>,
}

impl Driver {
    async fn run(mut self) {
        loop {
            self.flush().await;
            if self.tunnel.is_closed() {
                break;
            }
            let deadline = self.tunnel.poll_timeout();
            let sleep = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                command = self.commands.recv(), if self.commands_open => match command {
                    Some(Command::Send { cemi, reply }) => self.send(cemi, reply),
                    Some(Command::Close) => self.tunnel.close(Instant::now()),
                    None => {
                        // Every handle was dropped.
                        self.commands_open = false;
                        self.tunnel.close(Instant::now());
                    }
                },
                received = self.link.recv() => match received {
                    Ok(Received::Frame(source, frame)) => {
                        self.tunnel.handle_input(source, &frame, Instant::now());
                    }
                    Ok(Received::Closed) => self.transport_failed(),
                    // Over UDP, ICMP errors (e.g. port unreachable) surface here on Linux;
                    // the heartbeat notices a dead gateway, so just carry on.
                    Err(err) => {
                        tracing::debug!(%err, "receive failed");
                        if self.link.is_tcp() {
                            self.transport_failed();
                        }
                    }
                },
                () = sleep => self.tunnel.handle_timeout(Instant::now()),
            }
        }
        self.state.send_replace(ConnectionState::Closed);
    }

    fn send(&mut self, cemi: Cemi, reply: oneshot::Sender<Result<(), SendError>>) {
        let echo = match (&cemi, self.local_echo) {
            (Cemi::LDataReq(ldata), true) => Some(ldata.clone()),
            _ => None,
        };
        match self.tunnel.send(&cemi, Instant::now()) {
            Ok(id) => {
                self.pending.insert(id, (reply, echo));
            }
            Err(err) => {
                let _ = reply.send(Err(err));
            }
        }
    }

    /// The TCP stream is gone: tell the state machine and drop what was queued for it.
    fn transport_failed(&mut self) {
        self.link.reset();
        self.tunnel.transport_closed(Instant::now());
        while self.tunnel.poll_transmit().is_some() {}
    }

    async fn flush(&mut self) {
        while let Some(tx) = self.tunnel.poll_transmit() {
            if let Err(err) = self.link.send(&tx).await {
                tracing::debug!(%err, destination = %tx.destination, "send failed");
                if self.link.is_tcp() {
                    self.transport_failed();
                }
            }
        }
        while let Some(event) = self.tunnel.poll_event() {
            self.on_event(event);
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Connected { address, .. } => {
                self.state.send_replace(ConnectionState::Connected(address));
                if let Some(first) = self.first_connect.take() {
                    let _ = first.send(Ok(()));
                }
                tracing::info!(%address, "KNX tunnel connected");
                let _ = self.events.send(ClientEvent::Connected { address });
            }
            Event::Disconnected { reason, .. } => {
                // Over TCP, the next connect attempt needs a fresh stream.
                self.link.reset();
                self.state.send_replace(ConnectionState::Connecting);
                if let Some(first) = self.first_connect.take() {
                    // The first attempt failed: report it and stop retrying.
                    let _ = first.send(Err(reason));
                    self.tunnel.close(Instant::now());
                }
                tracing::warn!(%reason, "KNX tunnel disconnected");
                let _ = self.events.send(ClientEvent::Disconnected { reason });
            }
            Event::Received(Cemi::LDataInd(ldata)) => {
                let _ = self.events.send(ClientEvent::Frame(ldata));
            }
            Event::Received(other) => {
                let _ = self.events.send(ClientEvent::Other(other));
            }
            Event::SendResult { id, result } => {
                if let Some((reply, echo)) = self.pending.remove(&id) {
                    if let (Ok(()), Some(mut ldata)) = (&result, echo) {
                        if let Some(address) = self.tunnel.address() {
                            ldata.source = address;
                        }
                        let _ = self.events.send(ClientEvent::Frame(ldata));
                    }
                    let _ = reply.send(result);
                }
            }
            Event::Closed => {
                self.state.send_replace(ConnectionState::Closed);
                let _ = self.events.send(ClientEvent::Closed);
            }
        }
    }
}
