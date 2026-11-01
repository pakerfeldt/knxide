//! A KNXnet/IP tunnelling proxy: one tunnel to a real gateway (upstream), and a tunnelling
//! server for any number of clients (downstream).
//!
//! - Clients get their own individual address from a pool. Towards the bus, every frame
//!   carries the proxy's one upstream tunnel address.
//! - A client's `L_Data.req` goes to the bus; the client's `L_Data.con` waits for the real
//!   interface's confirmation. Once confirmed, the other clients see the frame as
//!   `L_Data.ind`, as they would with several tunnels on one interface.
//! - Bus traffic goes to every client.
//! - While the gateway is unreachable, clients are told (`E_KNX_CONNECTION` in heartbeats)
//!   and their frames are confirmed negatively at once.
//!
//! Group communication works fully. Management traffic to one client's address (e.g. ETS
//! programming through the proxy) does not: the bus only knows the upstream address.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use knxide_core::{Cemi, IndividualAddress, LData};
use knxide_ip::discovery::MULTICAST_GROUP;
use knxide_ip::server::{ServerConfig, ServerEvent, TunnelServer};
use knxide_tokio::{Client, ClientEvent, TunnelOptions};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

/// Proxy settings.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// The real gateway.
    pub gateway: SocketAddr,
    /// How to tunnel to it.
    pub upstream: TunnelOptions,
    /// Where to listen for clients. Default `0.0.0.0:3671`.
    pub listen: SocketAddr,
    /// The address clients should use, if it cannot be worked out (several interfaces).
    pub advertise: Option<Ipv4Addr>,
    /// Join the KNXnet/IP multicast group so clients can discover the proxy.
    pub discovery: bool,
    /// Name, client address pool and timers of the server side. `endpoint` is filled in.
    pub server: ServerConfig,
}

impl ProxyConfig {
    /// Defaults for proxying `gateway`.
    pub fn new(gateway: SocketAddr) -> Self {
        let mut server = ServerConfig::new(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
        server.name = "knxide proxy".to_string();
        Self {
            gateway,
            upstream: TunnelOptions::default(),
            listen: SocketAddr::from((Ipv4Addr::UNSPECIFIED, knxide_ip::frame::KNXNETIP_PORT)),
            advertise: None,
            discovery: true,
            server,
        }
    }
}

/// A running proxy.
pub struct Proxy {
    local_addr: SocketAddr,
    endpoint: SocketAddrV4,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    /// Bind the client-facing socket and start. Connecting upstream happens in the
    /// background, retrying until the gateway answers.
    pub async fn start(config: ProxyConfig) -> io::Result<Self> {
        let socket = bind(&config)?;
        let local_addr = socket.local_addr()?;
        let ip = match (config.advertise, local_addr.ip()) {
            (Some(ip), _) => ip,
            (None, IpAddr::V4(ip)) if !ip.is_unspecified() => ip,
            (None, _) => local_ip_towards(config.gateway).await?,
        };
        let endpoint = SocketAddrV4::new(ip, local_addr.port());
        let mut server_config = config.server.clone();
        server_config.endpoint = endpoint;
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run(
            socket,
            TunnelServer::new(server_config),
            config,
            stop_rx,
        ));
        Ok(Self {
            local_addr,
            endpoint,
            stop: Some(stop_tx),
            task,
        })
    }

    /// The socket clients talk to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The endpoint advertised to clients.
    pub fn endpoint(&self) -> SocketAddrV4 {
        self.endpoint
    }

    /// Disconnect every client and the upstream tunnel, then stop.
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }

    /// Run until `shutdown` completes, then stop.
    pub async fn run_until(self, shutdown: impl Future<Output = ()>) {
        shutdown.await;
        self.stop().await;
    }
}

fn bind(config: &ProxyConfig) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&config.listen.into())?;
    if config.discovery {
        let interface = match config.advertise {
            Some(ip) => ip,
            None => Ipv4Addr::UNSPECIFIED,
        };
        if let Err(err) = socket.join_multicast_v4(&MULTICAST_GROUP, &interface) {
            tracing::warn!(%err, "cannot join the KNXnet/IP multicast group; discovery by search will not find the proxy");
        }
    }
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

async fn local_ip_towards(target: SocketAddr) -> io::Result<Ipv4Addr> {
    let probe = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
    probe.connect(target).await?;
    match probe.local_addr()? {
        SocketAddr::V4(a) => Ok(*a.ip()),
        SocketAddr::V6(_) => Err(io::Error::new(io::ErrorKind::Unsupported, "IPv6")),
    }
}

enum Upstream {
    Connected(IndividualAddress),
    Down(String),
    Frame(LData),
}

/// Keep a tunnel to the gateway open, reporting state and frames.
async fn upstream(
    config: ProxyConfig,
    client_tx: watch::Sender<Option<Client>>,
    events: mpsc::Sender<Upstream>,
) {
    loop {
        let client = match Client::connect(config.gateway, config.upstream.clone()).await {
            Ok(client) => client,
            Err(err) => {
                let _ = events.send(Upstream::Down(err.to_string())).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let mut bus = client.subscribe();
        if let Some(address) = client.address() {
            let _ = events.send(Upstream::Connected(address)).await;
        }
        client_tx.send_replace(Some(client));
        loop {
            let event = match bus.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "upstream frames dropped");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let forwarded = match event {
                ClientEvent::Frame(ldata) => Upstream::Frame(ldata),
                ClientEvent::Connected { address } => Upstream::Connected(address),
                ClientEvent::Disconnected { reason } => Upstream::Down(reason.to_string()),
                ClientEvent::Other(_) => continue,
                ClientEvent::Closed => break,
            };
            if events.send(forwarded).await.is_err() {
                return;
            }
        }
        client_tx.send_replace(None);
    }
}

/// Send client frames to the bus one at a time, in order, and report each outcome.
async fn forwarder(
    client: watch::Receiver<Option<Client>>,
    mut requests: mpsc::Receiver<(u8, LData)>,
    done: mpsc::Sender<(u8, LData, bool)>,
) {
    while let Some((channel, ldata)) = requests.recv().await {
        let upstream = client.borrow().clone();
        let ok = match upstream {
            Some(client) => {
                let mut frame = ldata.clone();
                frame.source = IndividualAddress::UNSET;
                match client.send(Cemi::LDataReq(frame)).await {
                    Ok(()) => true,
                    Err(err) => {
                        tracing::debug!(channel, %err, "forwarding failed");
                        false
                    }
                }
            }
            None => false,
        };
        if done.send((channel, ldata, ok)).await.is_err() {
            return;
        }
    }
}

async fn run(
    socket: UdpSocket,
    mut server: TunnelServer,
    config: ProxyConfig,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) {
    let (client_tx, client_rx) = watch::channel(None);
    let (up_tx, mut up_rx) = mpsc::channel(1024);
    let (req_tx, req_rx) = mpsc::channel(1024);
    let (done_tx, mut done_rx) = mpsc::channel(1024);
    server.set_bus_connected(false);
    let upstream_task = tokio::spawn(upstream(config.clone(), client_tx, up_tx));
    let forwarder_task = tokio::spawn(forwarder(client_rx.clone(), req_rx, done_tx));
    let mut buf = vec![0u8; 2048];
    tracing::info!(endpoint = %server.config().endpoint, gateway = %config.gateway, "proxy listening");

    loop {
        flush(&socket, &mut server, &req_tx).await;
        let deadline = server.poll_timeout();
        let sleep = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at.into()).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            received = socket.recv_from(&mut buf) => match received {
                Ok((len, source)) => {
                    if let Some(datagram) = buf.get(..len) {
                        server.handle_input(source, datagram, Instant::now());
                    }
                }
                Err(err) => tracing::debug!(%err, "receive failed"),
            },
            Some(event) = up_rx.recv() => match event {
                Upstream::Connected(address) => {
                    tracing::info!(%address, "connected to the gateway");
                    server.set_bus_connected(true);
                }
                Upstream::Down(reason) => {
                    tracing::warn!(%reason, "gateway connection down");
                    server.set_bus_connected(false);
                }
                Upstream::Frame(ldata) => server.send_all(&Cemi::LDataInd(ldata), None, Instant::now()),
            },
            Some((channel, ldata, ok)) = done_rx.recv() => confirm(&mut server, channel, ldata, ok),
            () = sleep => server.handle_timeout(Instant::now()),
            _ = &mut stop => break,
        }
    }

    server.disconnect_all();
    flush(&socket, &mut server, &req_tx).await;
    upstream_task.abort();
    forwarder_task.abort();
    let upstream = client_rx.borrow().clone();
    if let Some(client) = upstream {
        client.close().await;
    }
}

/// Answer a client's frame: `L_Data.con` to it, and on success `L_Data.ind` to the others.
fn confirm(server: &mut TunnelServer, channel: u8, mut ldata: LData, ok: bool) {
    let now = Instant::now();
    if let Some(address) = server.client_address(channel) {
        ldata.source = address;
    }
    let mut con = ldata.clone();
    con.confirm_error = !ok;
    let _ = server.send(channel, &Cemi::LDataCon(con), now);
    if ok {
        server.send_all(&Cemi::LDataInd(ldata), Some(channel), now);
    }
}

async fn flush(
    socket: &UdpSocket,
    server: &mut TunnelServer,
    requests: &mpsc::Sender<(u8, LData)>,
) {
    loop {
        while let Some(tx) = server.poll_transmit() {
            if let Err(err) = socket.send_to(&tx.payload, tx.destination).await {
                tracing::debug!(%err, destination = %tx.destination, "send failed");
            }
        }
        let Some(event) = server.poll_event() else {
            return;
        };
        match event {
            ServerEvent::Connected {
                channel,
                address,
                endpoint,
            } => {
                tracing::info!(channel, %address, %endpoint, "client connected");
            }
            ServerEvent::Disconnected {
                channel,
                address,
                reason,
            } => {
                tracing::info!(channel, %address, ?reason, "client disconnected");
            }
            ServerEvent::Frame {
                channel,
                cemi: Cemi::LDataReq(ldata),
            } => {
                if requests.try_send((channel, ldata.clone())).is_err() {
                    // Too much at once: refuse rather than block every client.
                    confirm(server, channel, ldata, false);
                }
            }
            ServerEvent::Frame { channel, cemi } => {
                tracing::debug!(channel, ?cemi, "ignoring non-L_Data frame from client");
            }
        }
    }
}
