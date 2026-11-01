//! KNXnet/IP routing over multicast, behind the same [`Client`] handle as tunnelling.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Instant;

use knxide_core::Cemi;
use knxide_ip::router::{RouterClient, RouterConfig, RouterEvent};
use knxide_ip::tunnel::{SendError, SendId};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::client::{Client, ClientEvent, Command, ConnectionState, PendingSend};
use crate::error::Error;

/// Options for [`Client::routing`].
#[derive(Debug, Clone)]
pub struct RoutingOptions {
    /// Network interface (its IPv4 address) for multicast. Default: the one the OS routes
    /// `224.0.23.12` through.
    pub interface: Option<Ipv4Addr>,
    /// Receive our own frames back from the network stack. Default off.
    pub multicast_loop: bool,
    /// Multicast TTL. Default 16.
    pub ttl: u32,
    /// Report our own sent frames as [`ClientEvent::Frame`].
    pub local_echo: bool,
    /// Capacity of the event broadcast.
    pub event_capacity: usize,
    /// Address to bind when [`RouterConfig::group`] is not a multicast address (testing, or
    /// unicast to one router). Default `0.0.0.0:0`.
    pub bind: SocketAddr,
    /// Protocol settings: group, our individual address, rate limit.
    pub router: RouterConfig,
}

impl Default for RoutingOptions {
    fn default() -> Self {
        Self {
            interface: None,
            multicast_loop: false,
            ttl: 16,
            local_echo: false,
            event_capacity: 256,
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            router: RouterConfig::default(),
        }
    }
}

impl Client {
    /// Join KNXnet/IP routing. Only KNX IP routers take part; an IP interface does not route.
    /// The client is "connected" at once: routing has no connection.
    pub async fn routing(options: RoutingOptions) -> Result<Self, Error> {
        let (socket, own_endpoint) = routing_socket(&options).await?;
        let mut config = options.router.clone();
        config.own_endpoint = config.own_endpoint.or(own_endpoint);
        let address = config.individual_address;

        let (commands, command_rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(options.event_capacity.max(1));
        let (state_tx, state) = watch::channel(ConnectionState::Connected(address));
        let driver = RouterDriver {
            router: RouterClient::new(config),
            socket,
            commands: command_rx,
            events: events.clone(),
            state: state_tx,
            local_echo: options.local_echo,
            pending: HashMap::new(),
        };
        tokio::spawn(driver.run());
        Ok(Self {
            commands,
            events,
            state,
        })
    }
}

/// Bind and configure the socket. Returns it and our own endpoint, for loop filtering.
async fn routing_socket(
    options: &RoutingOptions,
) -> Result<(UdpSocket, Option<SocketAddr>), Error> {
    let group = options.router.group;
    let IpAddr::V4(group_ip) = group.ip() else {
        return Err(Error::Unsupported("IPv6"));
    };
    if !group_ip.is_multicast() {
        let socket = UdpSocket::bind(options.bind).await?;
        let own = socket.local_addr()?;
        return Ok((socket, Some(own)));
    }
    let interface = match options.interface {
        Some(ip) => ip,
        None => default_interface(group).await?,
    };
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Other KNX software on this host may hold port 3671 too.
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, group.port())).into())?;
    socket.join_multicast_v4(&group_ip, &interface)?;
    socket.set_multicast_if_v4(&interface)?;
    socket.set_multicast_loop_v4(options.multicast_loop)?;
    socket.set_multicast_ttl_v4(options.ttl)?;
    socket.set_nonblocking(true)?;
    let socket = UdpSocket::from_std(socket.into())?;
    let own = (!options.multicast_loop)
        .then_some(SocketAddr::V4(SocketAddrV4::new(interface, group.port())));
    Ok((socket, own))
}

async fn default_interface(towards: SocketAddr) -> Result<Ipv4Addr, Error> {
    let probe = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
    probe.connect(towards).await?;
    match probe.local_addr()? {
        SocketAddr::V4(local) => Ok(*local.ip()),
        SocketAddr::V6(_) => Err(Error::Unsupported("IPv6")),
    }
}

struct RouterDriver {
    router: RouterClient,
    socket: UdpSocket,
    commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<ClientEvent>,
    state: watch::Sender<ConnectionState>,
    local_echo: bool,
    pending: HashMap<SendId, PendingSend>,
}

impl RouterDriver {
    async fn run(mut self) {
        let mut buf = vec![0u8; 2048];
        loop {
            self.flush().await;
            let deadline = self.router.poll_timeout();
            let sleep = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(Command::Send { cemi, reply }) => self.send(cemi, reply),
                    Some(Command::Close) | None => break,
                },
                received = self.socket.recv_from(&mut buf) => match received {
                    Ok((len, source)) => {
                        if let Some(datagram) = buf.get(..len) {
                            self.router.handle_input(source, datagram, Instant::now());
                        }
                    }
                    Err(err) => tracing::debug!(%err, "receive failed"),
                },
                () = sleep => self.router.handle_timeout(Instant::now()),
            }
        }
        for (_, (reply, _)) in self.pending.drain() {
            let _ = reply.send(Err(SendError::Closed));
        }
        self.state.send_replace(ConnectionState::Closed);
        let _ = self.events.send(ClientEvent::Closed);
    }

    fn send(&mut self, cemi: Cemi, reply: oneshot::Sender<Result<(), SendError>>) {
        let echo = match (&cemi, self.local_echo) {
            (Cemi::LDataReq(ldata) | Cemi::LDataInd(ldata), true) => Some(ldata.clone()),
            _ => None,
        };
        match self.router.send(&cemi, Instant::now()) {
            Ok(id) => {
                self.pending.insert(id, (reply, echo));
            }
            Err(err) => {
                let _ = reply.send(Err(err));
            }
        }
    }

    async fn flush(&mut self) {
        while let Some(tx) = self.router.poll_transmit() {
            if let Err(err) = self.socket.send_to(&tx.payload, tx.destination).await {
                tracing::debug!(%err, destination = %tx.destination, "send failed");
            }
        }
        while let Some(event) = self.router.poll_event() {
            match event {
                RouterEvent::Received(Cemi::LDataInd(ldata)) => {
                    let _ = self.events.send(ClientEvent::Frame(ldata));
                }
                RouterEvent::Received(other) => {
                    let _ = self.events.send(ClientEvent::Other(other));
                }
                RouterEvent::Sent(id) => {
                    if let Some((reply, echo)) = self.pending.remove(&id) {
                        if let Some(mut ldata) = echo {
                            if ldata.source == knxide_core::IndividualAddress::UNSET {
                                ldata.source = self.router.config().individual_address;
                            }
                            let _ = self.events.send(ClientEvent::Frame(ldata));
                        }
                        let _ = reply.send(Ok(()));
                    }
                }
                RouterEvent::Busy { wait } => tracing::debug!(?wait, "router busy, pausing"),
                RouterEvent::LostMessages {
                    count,
                    device_state,
                } => {
                    tracing::warn!(count, device_state, "router lost messages");
                }
            }
        }
    }
}
