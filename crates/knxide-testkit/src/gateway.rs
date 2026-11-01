//! A fake KNXnet/IP tunnelling server.
//!
//! [`GatewayCore`] is the sans-IO server logic; [`FakeGateway`] runs it on a loopback UDP
//! socket in a background thread. The fake answers search, description, connect, heartbeat
//! and disconnect requests, acknowledges tunnelling requests, confirms `L_Data.req` with
//! `L_Data.con`, and simulates devices that answer `GroupValue_Read` for configured group
//! addresses. [`Behaviour`] injects faults.

use std::collections::HashMap;
use std::io;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use knxide_core::{Apdu, Cemi, Destination, GroupAddress, IndividualAddress, LData, Payload, Tpdu};
use knxide_ip::structures::Medium;
use knxide_ip::{Crd, Cri, DeviceInfo, Dib, Frame, Hpai, ServiceFamily, Status};

/// Fault injection and simulated behaviour. Change it with [`FakeGateway::behaviour`].
#[derive(Debug, Clone)]
pub struct Behaviour {
    /// Refuse connect requests with this status.
    pub refuse_connections: Option<Status>,
    /// Most simultaneous tunnels; further connects get `E_NO_MORE_CONNECTIONS`.
    pub max_tunnels: usize,
    /// Do not answer heartbeats.
    pub ignore_heartbeats: bool,
    /// Do not acknowledge the next this many tunnelling requests.
    pub drop_acks: u32,
    /// Send `L_Data.con` for `L_Data.req`.
    pub send_confirmations: bool,
    /// Confirm with the error flag set.
    pub confirm_errors: bool,
    /// Answer `GroupValue_Read` for addresses with a value (see [`FakeGateway::set_value`]).
    pub answer_reads: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            refuse_connections: None,
            max_tunnels: 4,
            ignore_heartbeats: false,
            drop_acks: 0,
            send_confirmations: true,
            confirm_errors: false,
            answer_reads: true,
        }
    }
}

#[derive(Debug)]
struct Tunnel {
    control: SocketAddr,
    data: SocketAddr,
    address: IndividualAddress,
    send_sequence: u8,
    recv_sequence: u8,
}

/// Sans-IO server logic. Feed it frames, get back frames to send.
#[derive(Debug)]
pub struct GatewayCore {
    /// Behaviour knobs.
    pub behaviour: Behaviour,
    /// The gateway's own address, used as `L_Data.con` source and in descriptions.
    pub address: IndividualAddress,
    /// Address of the simulated device answering reads.
    pub device_address: IndividualAddress,
    /// Name in the device information DIB.
    pub name: String,
    /// Serving over TCP: no tunnelling acks, no sequence checks.
    pub tcp: bool,
    tunnels: HashMap<u8, Tunnel>,
    next_channel: u8,
    values: HashMap<GroupAddress, Payload>,
    received: Vec<Cemi>,
    restarts: Vec<IndividualAddress>,
    frames: Vec<Frame>,
    control_endpoint: SocketAddrV4,
}

impl GatewayCore {
    /// A gateway whose control endpoint is `control_endpoint`.
    pub fn new(control_endpoint: SocketAddrV4) -> Self {
        Self {
            behaviour: Behaviour::default(),
            address: IndividualAddress::from_raw(0x1100),
            device_address: IndividualAddress::from_raw(0x110A),
            name: "knxide fake gateway".to_string(),
            tcp: false,
            tunnels: HashMap::new(),
            next_channel: 1,
            values: HashMap::new(),
            received: Vec::new(),
            restarts: Vec::new(),
            frames: Vec::new(),
            control_endpoint,
        }
    }

    fn dibs(&self) -> Vec<Dib> {
        vec![
            Dib::DeviceInfo(DeviceInfo {
                medium: Medium::TP1,
                status: 0,
                address: self.address,
                project_installation_id: 0,
                serial_number: [0x00, 0x00, 0x6B, 0x6E, 0x78, 0x01],
                multicast_address: Ipv4Addr::UNSPECIFIED,
                mac_address: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
                name: self.name.clone(),
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

    /// Handle one frame from `source`. Returns frames to send and where.
    pub fn handle(&mut self, source: SocketAddr, frame: Frame) -> Vec<(SocketAddr, Frame)> {
        self.frames.push(frame.clone());
        let mut out = Vec::new();
        match frame {
            Frame::SearchRequest { discovery_endpoint } => out.push((
                discovery_endpoint.resolve(source),
                Frame::SearchResponse {
                    control_endpoint: Hpai::udp(self.control_endpoint),
                    dibs: self.dibs(),
                },
            )),
            Frame::DescriptionRequest { control_endpoint } => {
                out.push((
                    control_endpoint.resolve(source),
                    Frame::DescriptionResponse { dibs: self.dibs() },
                ));
            }
            Frame::ConnectRequest {
                control_endpoint,
                data_endpoint,
                cri,
            } => {
                let reply_to = control_endpoint.resolve(source);
                let refuse = |status| Frame::ConnectResponse {
                    channel_id: 0,
                    status,
                    data_endpoint: None,
                    crd: None,
                };
                if let Some(status) = self.behaviour.refuse_connections {
                    out.push((reply_to, refuse(status)));
                } else if !matches!(cri, Cri::Tunnel { .. }) {
                    out.push((reply_to, refuse(Status::CONNECTION_TYPE)));
                } else if self.tunnels.len() >= self.behaviour.max_tunnels {
                    out.push((reply_to, refuse(Status::NO_MORE_CONNECTIONS)));
                } else {
                    let channel_id = self.allocate_channel();
                    let address = IndividualAddress::from_raw(0x11FA + u16::from(channel_id));
                    self.tunnels.insert(
                        channel_id,
                        Tunnel {
                            control: reply_to,
                            data: data_endpoint.resolve(source),
                            address,
                            send_sequence: 0,
                            recv_sequence: 0,
                        },
                    );
                    out.push((
                        reply_to,
                        Frame::ConnectResponse {
                            channel_id,
                            status: Status::NO_ERROR,
                            data_endpoint: Some(if self.tcp {
                                Hpai::TCP
                            } else {
                                Hpai::udp(self.control_endpoint)
                            }),
                            crd: Some(Crd::Tunnel { address }),
                        },
                    ));
                }
            }
            Frame::ConnectionStateRequest {
                channel_id,
                control_endpoint,
            } => {
                if !self.behaviour.ignore_heartbeats {
                    let status = if self.tunnels.contains_key(&channel_id) {
                        Status::NO_ERROR
                    } else {
                        Status::CONNECTION_ID
                    };
                    out.push((
                        control_endpoint.resolve(source),
                        Frame::ConnectionStateResponse { channel_id, status },
                    ));
                }
            }
            Frame::DisconnectRequest {
                channel_id,
                control_endpoint,
            } => {
                self.tunnels.remove(&channel_id);
                out.push((
                    control_endpoint.resolve(source),
                    Frame::DisconnectResponse {
                        channel_id,
                        status: Status::NO_ERROR,
                    },
                ));
            }
            Frame::TunnellingRequest {
                channel_id,
                sequence,
                cemi,
            } => {
                self.on_tunnelling_request(channel_id, sequence, &cemi, &mut out);
            }
            _ => {}
        }
        out
    }

    fn allocate_channel(&mut self) -> u8 {
        loop {
            let id = self.next_channel;
            self.next_channel = self.next_channel.wrapping_add(1).max(1);
            if !self.tunnels.contains_key(&id) {
                return id;
            }
        }
    }

    fn on_tunnelling_request(
        &mut self,
        channel_id: u8,
        sequence: u8,
        cemi: &[u8],
        out: &mut Vec<(SocketAddr, Frame)>,
    ) {
        let Some(tunnel) = self.tunnels.get_mut(&channel_id) else {
            return;
        };
        if self.behaviour.drop_acks > 0 {
            self.behaviour.drop_acks -= 1;
            return;
        }
        let data = tunnel.data;
        let tunnel_address = tunnel.address;
        if !self.tcp {
            let ack = Frame::TunnellingAck {
                channel_id,
                sequence,
                status: Status::NO_ERROR,
            };
            if sequence == tunnel.recv_sequence.wrapping_sub(1) {
                out.push((data, ack));
                return;
            }
            if sequence != tunnel.recv_sequence {
                return;
            }
            tunnel.recv_sequence = tunnel.recv_sequence.wrapping_add(1);
            out.push((data, ack));
        }

        let Ok(Cemi::LDataReq(mut ldata)) = Cemi::decode(cemi) else {
            return;
        };
        ldata.source = tunnel_address;
        self.received.push(Cemi::LDataReq(ldata.clone()));
        // Individually addressed frames are acknowledged on the bus only by an existing device;
        // without that, the interface confirms with an error.
        let unknown_device =
            matches!(ldata.destination, Destination::Individual(ia) if ia != self.device_address);
        let failed = self.behaviour.confirm_errors || unknown_device;
        if self.behaviour.send_confirmations {
            let mut con = ldata.clone();
            con.confirm_error = failed;
            self.send_to_tunnel(channel_id, &Cemi::LDataCon(con), out);
        }
        if failed {
            return;
        }
        // The simulated device acknowledges connected data and records restarts.
        if let Tpdu::Connected { sequence, apdu } = &ldata.tpdu {
            if *apdu == Apdu::restart() {
                self.restarts.push(self.device_address);
            }
            let mut ack = LData::new(
                tunnel_address,
                Tpdu::Ack {
                    sequence: *sequence,
                },
            );
            ack.source = self.device_address;
            self.send_to_tunnel(channel_id, &Cemi::LDataInd(ack), out);
        }
        // Other tunnels see the frame on the bus.
        let others: Vec<u8> = self
            .tunnels
            .keys()
            .copied()
            .filter(|&c| c != channel_id)
            .collect();
        for other in others {
            self.send_to_tunnel(other, &Cemi::LDataInd(ldata.clone()), out);
        }
        // Simulated devices.
        let Some(ga) = ldata.group_address() else {
            return;
        };
        match ldata.apdu() {
            Some(Apdu::GroupValueWrite(payload)) => {
                self.values.insert(ga, payload.clone());
            }
            Some(Apdu::GroupValueRead) if self.behaviour.answer_reads => {
                if let Some(value) = self.values.get(&ga).cloned() {
                    let mut response = LData::group(ga, Apdu::GroupValueResponse(value));
                    response.source = self.device_address;
                    self.indicate(&response, out);
                }
            }
            _ => {}
        }
    }

    /// Send an `L_Data.ind` to every tunnel, as if `ldata` appeared on the bus.
    pub fn indicate(&mut self, ldata: &LData, out: &mut Vec<(SocketAddr, Frame)>) {
        let channels: Vec<u8> = self.tunnels.keys().copied().collect();
        for channel_id in channels {
            self.send_to_tunnel(channel_id, &Cemi::LDataInd(ldata.clone()), out);
        }
    }

    fn send_to_tunnel(&mut self, channel_id: u8, cemi: &Cemi, out: &mut Vec<(SocketAddr, Frame)>) {
        let Some(tunnel) = self.tunnels.get_mut(&channel_id) else {
            return;
        };
        let Ok(bytes) = cemi.to_vec() else { return };
        let sequence = tunnel.send_sequence;
        tunnel.send_sequence = tunnel.send_sequence.wrapping_add(1);
        out.push((
            tunnel.data,
            Frame::TunnellingRequest {
                channel_id,
                sequence,
                cemi: bytes,
            },
        ));
    }

    /// Server-initiated disconnect of every tunnel.
    pub fn disconnect_all(&mut self, out: &mut Vec<(SocketAddr, Frame)>) {
        for (channel_id, tunnel) in self.tunnels.drain() {
            out.push((
                tunnel.control,
                Frame::DisconnectRequest {
                    channel_id,
                    control_endpoint: Hpai::udp(self.control_endpoint),
                },
            ));
        }
    }
}

/// TCP streams by peer address, so frames for a tunnel go to its connection.
type Streams = Arc<Mutex<HashMap<SocketAddr, TcpStream>>>;

enum Io {
    Udp(UdpSocket),
    Tcp(Streams),
}

/// [`GatewayCore`] on a loopback UDP socket or TCP listener, in background threads. Stops
/// when dropped.
pub struct FakeGateway {
    address: SocketAddr,
    io: Io,
    core: Arc<Mutex<GatewayCore>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl FakeGateway {
    /// Start on a free loopback port.
    pub fn spawn() -> io::Result<Self> {
        Self::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
    }

    /// Start on `address` (e.g. to restart on the port a previous instance used).
    pub fn bind(address: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(address)?;
        socket.set_read_timeout(Some(Duration::from_millis(5)))?;
        let address = socket.local_addr()?;
        let SocketAddr::V4(v4) = address else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "IPv4 only"));
        };
        let core = Arc::new(Mutex::new(GatewayCore::new(v4)));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let socket = socket.try_clone()?;
            let core = Arc::clone(&core);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || serve(&socket, &core, &stop))
        };
        Ok(Self {
            address,
            io: Io::Udp(socket),
            core,
            stop,
            threads: vec![thread],
        })
    }

    /// Start a TCP (tunnelling v2) server on a free loopback port.
    pub fn spawn_tcp() -> io::Result<Self> {
        Self::bind_tcp(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
    }

    /// Start a TCP server on `address`.
    pub fn bind_tcp(address: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let SocketAddr::V4(v4) = address else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "IPv4 only"));
        };
        let mut core = GatewayCore::new(v4);
        core.tcp = true;
        let core = Arc::new(Mutex::new(core));
        let stop = Arc::new(AtomicBool::new(false));
        let streams: Streams = Arc::default();
        let thread = {
            let core = Arc::clone(&core);
            let stop = Arc::clone(&stop);
            let streams = Arc::clone(&streams);
            std::thread::spawn(move || accept(&listener, &core, &stop, &streams))
        };
        Ok(Self {
            address,
            io: Io::Tcp(streams),
            core,
            stop,
            threads: vec![thread],
        })
    }

    /// The control endpoint to connect to.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    fn core(&self) -> MutexGuard<'_, GatewayCore> {
        self.core
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Change behaviour.
    pub fn behaviour(&self, change: impl FnOnce(&mut Behaviour)) {
        change(&mut self.core().behaviour);
    }

    /// Set the value a simulated device returns for reads of `ga`.
    pub fn set_value(&self, ga: GroupAddress, payload: Payload) {
        self.core().values.insert(ga, payload);
    }

    /// The last value written to (or set for) `ga`.
    pub fn value(&self, ga: GroupAddress) -> Option<Payload> {
        self.core().values.get(&ga).cloned()
    }

    /// `L_Data.req` frames received from clients, with the source filled in.
    pub fn received(&self) -> Vec<Cemi> {
        self.core().received.clone()
    }

    /// Devices that received a basic `A_Restart`, in order.
    pub fn restarts(&self) -> Vec<IndividualAddress> {
        self.core().restarts.clone()
    }

    /// Every KNXnet/IP frame received.
    pub fn frames(&self) -> Vec<Frame> {
        self.core().frames.clone()
    }

    /// Number of open tunnels.
    pub fn tunnel_count(&self) -> usize {
        self.core().tunnels.len()
    }

    /// Put a frame on the "bus": every tunnel gets it as `L_Data.ind`.
    pub fn indicate(&self, ldata: &LData) {
        let mut out = Vec::new();
        self.core().indicate(ldata, &mut out);
        self.send_all(out);
    }

    /// Disconnect every tunnel from the server side.
    pub fn disconnect_all(&self) {
        let mut out = Vec::new();
        self.core().disconnect_all(&mut out);
        self.send_all(out);
    }

    fn send_all(&self, out: Vec<(SocketAddr, Frame)>) {
        match &self.io {
            Io::Udp(socket) => send_all(socket, out),
            Io::Tcp(streams) => send_all_tcp(streams, out),
        }
    }

    /// Close every TCP connection from the server side.
    pub fn drop_connections(&self) {
        if let Io::Tcp(streams) = &self.io {
            for (_, stream) in lock(streams).drain() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    /// Stop serving and release the port.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.drop_connections();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for FakeGateway {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn send_all(socket: &UdpSocket, out: Vec<(SocketAddr, Frame)>) {
    for (destination, frame) in out {
        if let Ok(bytes) = frame.to_vec() {
            let _ = socket.send_to(&bytes, destination);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn send_all_tcp(streams: &Streams, out: Vec<(SocketAddr, Frame)>) {
    let mut streams = lock(streams);
    for (destination, frame) in out {
        if let (Some(stream), Ok(bytes)) = (streams.get_mut(&destination), frame.to_vec()) {
            let _ = stream.write_all(&bytes);
        }
    }
}

fn accept(
    listener: &TcpListener,
    core: &Arc<Mutex<GatewayCore>>,
    stop: &Arc<AtomicBool>,
    streams: &Streams,
) {
    let mut readers = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let Ok((stream, peer)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        let (Ok(()), Ok(writer)) = (stream.set_nonblocking(false), stream.try_clone()) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(5)));
        lock(streams).insert(peer, writer);
        let (core, stop, streams) = (Arc::clone(core), Arc::clone(stop), Arc::clone(streams));
        readers.push(std::thread::spawn(move || {
            serve_tcp(stream, peer, &core, &stop, &streams)
        }));
    }
    for reader in readers {
        let _ = reader.join();
    }
}

fn serve_tcp(
    mut stream: TcpStream,
    peer: SocketAddr,
    core: &Mutex<GatewayCore>,
    stop: &AtomicBool,
    streams: &Streams,
) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => break,
        }
        while let Ok(Some(len)) = knxide_ip::frame::frame_len(&buf) {
            if buf.len() < len {
                break;
            }
            let bytes: Vec<u8> = buf.drain(..len).collect();
            if let Ok(frame) = Frame::decode(&bytes) {
                let out = lock(core).handle(peer, frame);
                send_all_tcp(streams, out);
            }
        }
    }
    lock(streams).remove(&peer);
}

fn serve(socket: &UdpSocket, core: &Mutex<GatewayCore>, stop: &AtomicBool) {
    let mut buf = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        let Ok((len, source)) = socket.recv_from(&mut buf) else {
            continue;
        };
        let Some(datagram) = buf.get(..len) else {
            continue;
        };
        let Ok(frame) = Frame::decode(datagram) else {
            continue;
        };
        let out = core
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .handle(source, frame);
        send_all(socket, out);
    }
}
