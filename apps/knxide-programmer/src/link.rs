//! How frames reach a device and come back: a tunnel, routing, or a simulated bus.

use std::fmt;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};

use knxide_core::{Destination, IndividualAddress, Tpdu};
use knxide_ip::router::RouterConfig;
use knxide_ip::tunnel::Transport;
use knxide_tokio::{Client, ClientEvent, RoutingOptions, TunnelOptions};
use tokio::sync::broadcast;

use crate::interfaces::{self, Kind};

/// IP time-to-live of routing frames: they stay on the local network. The recorded tool
/// sends with the same value.
const ROUTING_TTL: u32 = 1;

/// A frame could not be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// The interface reports that no device acknowledged the frame on the bus.
    NotAcknowledged,
    /// Anything else: the connection to the bus is gone, the frame cannot be encoded.
    Failed(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAcknowledged => f.write_str("no device acknowledged the frame"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// A way to exchange transport layer frames with devices by individual address.
pub trait Link: Send + 'static {
    /// The tool's own individual address on this link.
    fn address(&self) -> IndividualAddress;

    /// Send `tpdu` to `device`.
    fn send(
        &mut self,
        device: IndividualAddress,
        tpdu: Tpdu,
    ) -> impl Future<Output = Result<(), LinkError>> + Send;

    /// The next frame addressed to the tool, with the device it comes from; `None` when the
    /// link is gone. Must be safe to drop and call again without losing a frame.
    fn receive(&mut self) -> impl Future<Output = Option<(IndividualAddress, Tpdu)>> + Send;

    /// Give the link up.
    fn close(self) -> impl Future<Output = ()> + Send;
}

/// What to tell someone about to download over a wireless interface.
pub const WIRELESS_WARNING: &str = "Routing runs over Wi-Fi. Multicast over Wi-Fi loses frames, \
    so a download is likely to stop partway and leave the device without an application. Use a \
    wired connection for downloads. Comparing is not hindered: it reads again what it misses.";

/// What to tell someone when routing has no interface to run on.
pub const NO_NETWORK_WARNING: &str = "This computer is not on a network, so the devices cannot \
    be reached. Connect it, with a cable for downloads, and try again.";

/// How the bus is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusAccess {
    /// A KNXnet/IP tunnel to an interface or router.
    Tunnel {
        /// The gateway's control endpoint.
        gateway: SocketAddr,
        /// Tunnel over TCP instead of UDP.
        tcp: bool,
    },
    /// KNXnet/IP routing: frames are multicast and every KNX IP router on the network
    /// forwards them.
    Routing {
        /// Local interface (its IPv4 address) the multicast goes out on.
        interface: Ipv4Addr,
        /// The interface's name, `enp3s0`; empty when only its address is known.
        name: String,
        /// Whether the interface is wired or wireless. Multicast over Wi-Fi loses frames.
        kind: Kind,
        /// The tool's own individual address.
        address: IndividualAddress,
        /// Where frames go instead of the routing multicast group; for tests, which reach a
        /// stand-in router by unicast.
        group: Option<SocketAddr>,
    },
}

impl BusAccess {
    /// Routing from `address` on the interface that is chosen at this moment: a wired one
    /// that is up before a wireless one, see [`interfaces::choose`]. `None` when this machine
    /// has no interface to route on. Nothing is sent.
    pub fn routing_now(address: IndividualAddress) -> Option<Self> {
        let interface = interfaces::choose(&interfaces::list(), interfaces::system_default())?;
        Some(Self::Routing {
            interface: interface.address,
            name: interface.name,
            kind: interface.kind,
            address,
            group: None,
        })
    }

    /// Whether frames travel as multicast over Wi-Fi.
    pub fn is_wireless(&self) -> bool {
        matches!(
            self,
            Self::Routing {
                kind: Kind::Wireless,
                ..
            }
        )
    }
}

impl fmt::Display for BusAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tunnel { gateway, tcp } => {
                write!(f, "tunnel to {gateway}{}", if *tcp { " (TCP)" } else { "" })
            }
            Self::Routing {
                interface,
                name,
                kind,
                address,
                ..
            } => {
                if name.is_empty() {
                    write!(f, "routing on {interface} ({kind}) as {address}")
                } else {
                    write!(f, "routing on {name} ({kind}) as {address}")
                }
            }
        }
    }
}

/// A [`Link`] over the KNXnet/IP client: a tunnel or routing.
///
/// Frames go out at system priority, without repetition on the medium and with hop count 6;
/// over routing that makes the control octets `B0 60` of a cEMI `L_Data.ind`, as the recorded
/// tool sends them.
pub struct ClientLink {
    client: Client,
    events: broadcast::Receiver<ClientEvent>,
    address: IndividualAddress,
}

impl ClientLink {
    /// Open the bus access. A tunnel is connected and its address taken as the tool's; routing
    /// is joined with the configured address.
    pub async fn open(access: &BusAccess) -> Result<Self, String> {
        let client = match access {
            BusAccess::Tunnel { gateway, tcp } => {
                let mut options = TunnelOptions::default();
                if *tcp {
                    options.tunnel.transport = Transport::Tcp;
                }
                // One operation, one tunnel: a lost tunnel ends the operation.
                Client::connect(*gateway, options)
                    .await
                    .map_err(|error| format!("cannot open a tunnel to {gateway}: {error}"))?
            }
            BusAccess::Routing {
                interface,
                address,
                group,
                ..
            } => {
                let mut router = RouterConfig {
                    individual_address: *address,
                    ..RouterConfig::default()
                };
                let mut options = RoutingOptions {
                    interface: Some(*interface),
                    ttl: ROUTING_TTL,
                    ..RoutingOptions::default()
                };
                if let Some(group) = group {
                    router.group = *group;
                    options.bind = SocketAddr::from((*interface, 0));
                }
                options.router = router;
                Client::routing(options)
                    .await
                    .map_err(|error| format!("cannot join routing on {interface}: {error}"))?
            }
        };
        // Subscribe before anything is sent, so that no answer is missed.
        let events = client.subscribe();
        let Some(address) = client.address() else {
            client.close().await;
            return Err("the bus connection closed at once".into());
        };
        Ok(Self {
            client,
            events,
            address,
        })
    }
}

impl Link for ClientLink {
    fn address(&self) -> IndividualAddress {
        self.address
    }

    async fn send(&mut self, device: IndividualAddress, tpdu: Tpdu) -> Result<(), LinkError> {
        match self.client.send_to_device(device, tpdu).await {
            Ok(()) => Ok(()),
            Err(knxide_tokio::Error::DeviceNotResponding(_)) => Err(LinkError::NotAcknowledged),
            Err(error) => Err(LinkError::Failed(error.to_string())),
        }
    }

    async fn receive(&mut self) -> Option<(IndividualAddress, Tpdu)> {
        loop {
            match self.events.recv().await {
                Ok(ClientEvent::Frame(frame))
                    if frame.destination == Destination::Individual(self.address) =>
                {
                    return Some((frame.source, frame.tpdu));
                }
                Ok(ClientEvent::Frame(_) | ClientEvent::Other(_)) => {}
                Ok(ClientEvent::Connected { .. }) => {}
                // A tunnel that drops takes the device connection's timing with it.
                Ok(ClientEvent::Disconnected { .. } | ClientEvent::Closed)
                | Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "bus frames dropped; an answer may be among them");
                }
            }
        }
    }

    async fn close(self) {
        self.client.close().await;
    }
}
