//! The KNX connection the server holds, and what it has seen on the bus.
//!
//! The tunnel is opened (or routing joined) on first use or at start-up, and kept; a tunnel
//! reconnects on its own after losses. Telegrams seen since then are kept for `recent_telegrams` and to show
//! each group address's last value.

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use knxide_core::dpt::DptId;
use knxide_core::{Apdu, GroupAddress, IndividualAddress, LData, Payload};
use knxide_ip::tunnel::Transport;
use knxide_project::GroupAddressTable;
use knxide_tokio::{Client, ClientEvent, ConnectionState, RoutingOptions, TunnelOptions};
use tokio::sync::broadcast;

/// Group telegrams kept for `recent_telegrams`.
pub const HISTORY: usize = 500;

/// A group telegram seen on the bus (or sent by us).
#[derive(Debug, Clone)]
pub struct Seen {
    /// Milliseconds since the Unix epoch.
    pub time_ms: u64,
    /// Sender.
    pub source: IndividualAddress,
    /// Group address.
    pub address: GroupAddress,
    /// `write`, `response` or `read`.
    pub service: &'static str,
    /// The value, for writes and responses.
    pub payload: Option<Payload>,
}

/// How to reach the bus.
#[derive(Debug, Clone)]
pub enum Link {
    /// Tunnelling to a KNX IP interface or router.
    Tunnel {
        /// `IP` or `IP:PORT` (default port 3671).
        gateway: String,
        /// UDP or TCP.
        transport: Transport,
    },
    /// KNXnet/IP routing (multicast), through any KNX IP router on the network. Takes no
    /// tunnel slot.
    Routing(RoutingOptions),
}

/// Shared connection and bus state.
#[derive(Clone)]
pub struct Bus {
    inner: Arc<Inner>,
}

struct Inner {
    link: Option<Link>,
    project: GroupAddressTable,
    project_file: Option<String>,
    client: Mutex<Option<Client>>,
    /// Held while connecting, so concurrent tool calls share one attempt. Separate from
    /// `client` so status checks do not wait for a connect.
    connecting: tokio::sync::Mutex<()>,
    seen: Mutex<SeenState>,
}

#[derive(Default)]
struct SeenState {
    history: VecDeque<Seen>,
    /// Last write or response per group address.
    last: BTreeMap<GroupAddress, Seen>,
}

impl Bus {
    /// A bus reached through `link` (connected when first needed), or none at all.
    pub fn new(
        link: Option<Link>,
        project: GroupAddressTable,
        project_file: Option<String>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                link,
                project,
                project_file,
                client: Mutex::new(None),
                connecting: tokio::sync::Mutex::new(()),
                seen: Mutex::new(SeenState::default()),
            }),
        }
    }

    /// The group address table, with DPT overrides applied.
    pub fn project(&self) -> &GroupAddressTable {
        &self.inner.project
    }

    /// The project file's name, if one was loaded.
    pub fn project_file(&self) -> Option<&str> {
        self.inner.project_file.as_deref()
    }

    /// How the bus is reached.
    pub fn link(&self) -> Option<&Link> {
        self.inner.link.as_ref()
    }

    /// The connection state without connecting: `None` if no connection was attempted yet.
    pub fn state(&self) -> Option<ConnectionState> {
        self.slot().as_ref().map(Client::state)
    }

    /// Whether a first connect attempt is under way.
    pub fn connecting(&self) -> bool {
        self.inner.connecting.try_lock().is_err()
    }

    fn open_client(&self) -> Option<Client> {
        self.slot()
            .as_ref()
            .filter(|c| c.state() != ConnectionState::Closed)
            .cloned()
    }

    /// The open client, connecting first if needed.
    pub async fn client(&self) -> Result<Client, String> {
        if let Some(client) = self.open_client() {
            return Ok(client);
        }
        let _connecting = self.inner.connecting.lock().await;
        // Another call may have connected while we waited.
        if let Some(client) = self.open_client() {
            return Ok(client);
        }
        let client = match &self.inner.link {
            None => {
                return Err("no bus configured: start knxide-mcp with --gateway IP (or \
                            KNXIDE_GATEWAY), or with --routing (or KNXIDE_ROUTING=1)"
                    .into());
            }
            Some(Link::Tunnel { gateway, transport }) => {
                let address = parse_endpoint(gateway)?;
                let mut options = TunnelOptions {
                    local_echo: true,
                    ..TunnelOptions::default()
                };
                options.tunnel.transport = *transport;
                let client = Client::connect(address, options)
                    .await
                    .map_err(|e| format!("cannot reach the gateway at {address}: {e}"))?;
                tracing::info!(%address, "connected");
                client
            }
            Some(Link::Routing(options)) => {
                let options = RoutingOptions {
                    local_echo: true,
                    ..options.clone()
                };
                let client = Client::routing(options.clone())
                    .await
                    .map_err(|e| format!("cannot join KNXnet/IP routing: {e}"))?;
                tracing::info!(
                    group = %options.router.group,
                    address = %options.router.individual_address,
                    interface = ?options.interface,
                    "routing joined"
                );
                client
            }
        };
        let events = client.subscribe();
        tokio::spawn(self.clone().listen(events));
        *self.slot() = Some(client.clone());
        Ok(client)
    }

    /// Close the connection, if any.
    pub async fn close(&self) {
        let client = self.slot().take();
        if let Some(client) = client {
            client.close().await;
        }
    }

    async fn listen(self, mut events: broadcast::Receiver<ClientEvent>) {
        loop {
            match events.recv().await {
                Ok(ClientEvent::Frame(ldata)) => self.record(&ldata),
                Ok(ClientEvent::Connected { address }) => {
                    tracing::info!(%address, "tunnel open");
                }
                Ok(ClientEvent::Disconnected { reason }) => {
                    tracing::warn!(%reason, "tunnel lost; reconnecting");
                }
                Ok(ClientEvent::Other(_)) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "bus events dropped");
                }
                Ok(ClientEvent::Closed) | Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    fn record(&self, ldata: &LData) {
        let Some(address) = ldata.group_address() else {
            return;
        };
        let (service, payload) = match ldata.apdu() {
            Some(Apdu::GroupValueWrite(p)) => ("write", Some(p.clone())),
            Some(Apdu::GroupValueResponse(p)) => ("response", Some(p.clone())),
            Some(Apdu::GroupValueRead) => ("read", None),
            _ => return,
        };
        let seen = Seen {
            time_ms: now_ms(),
            source: ldata.source,
            address,
            service,
            payload,
        };
        let mut state = self.seen();
        if seen.payload.is_some() {
            state.last.insert(address, seen.clone());
        }
        if state.history.len() >= HISTORY {
            state.history.pop_front();
        }
        state.history.push_back(seen);
    }

    /// The last value seen for `address`.
    pub fn last(&self, address: GroupAddress) -> Option<Seen> {
        self.seen().last.get(&address).cloned()
    }

    /// The most recent `limit` telegrams, optionally for one address, oldest first.
    pub fn recent(&self, address: Option<GroupAddress>, limit: usize) -> Vec<Seen> {
        let state = self.seen();
        let mut out: Vec<Seen> = state
            .history
            .iter()
            .rev()
            .filter(|s| address.is_none_or(|a| s.address == a))
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        out
    }

    /// `GroupValue_Read`, returning the responder and payload.
    pub async fn read(
        &self,
        address: GroupAddress,
        timeout: Duration,
    ) -> Result<(IndividualAddress, Payload), String> {
        let client = self.client().await?;
        client
            .group_read_from(address, timeout)
            .await
            .map_err(|e| match e {
                knxide_tokio::Error::Timeout => format!(
                    "no device answered the read of {address} within {} ms (the address may \
                     have no device with the read flag set)",
                    timeout.as_millis()
                ),
                e => format!("read of {address} failed: {e}"),
            })
    }

    /// `GroupValue_Write`.
    pub async fn write(&self, address: GroupAddress, payload: Payload) -> Result<(), String> {
        let client = self.client().await?;
        client
            .group_write(address, payload)
            .await
            .map_err(|e| format!("write to {address} failed: {e}"))
    }

    /// The DPT for `address` from the project.
    pub fn dpt(&self, address: GroupAddress) -> Option<DptId> {
        self.inner.project.dpt(address)
    }

    fn slot(&self) -> MutexGuard<'_, Option<Client>> {
        self.inner
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn seen(&self) -> MutexGuard<'_, SeenState> {
        self.inner
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// `IP` or `IP:PORT`; the port defaults to 3671.
fn parse_endpoint(text: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    text.parse::<SocketAddr>()
        .or_else(|_| {
            text.parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, knxide_ip::frame::KNXNETIP_PORT))
        })
        .map_err(|_| format!("gateway {text:?} is not an IP address"))
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
