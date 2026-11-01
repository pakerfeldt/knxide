//! The one KNX connection the server holds, shared by every browser tab.
//!
//! Interfaces offer only a few tunnels, so tabs share a connection instead of opening one
//! each. State changes and bus frames go to every tab through a broadcast channel.

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use knxide_core::dpt::DptId;
use knxide_core::{Apdu, GroupAddress, IndividualAddress, LData, Payload, Tpdu};
use knxide_ip::tunnel::{SendError, Transport as TunnelTransport};
use knxide_project::{GroupAddressInfo, GroupAddressTable};
use knxide_tokio::{Client, ClientEvent, TunnelOptions};
use tokio::sync::broadcast;

use crate::health::{Health, HealthConfig, value_warning};
use crate::protocol::{
    ConnState, Device, GaEntry, Kind, ProjectInfo, ServerMsg, Status, Telegram, Transport,
};

/// Telegrams kept for tabs that open later.
const HISTORY: usize = 1000;
/// How long a read waits for the response.
const READ_TIMEOUT: Duration = Duration::from_secs(3);

/// Shared application state.
#[derive(Clone)]
pub struct Bus {
    inner: Arc<Mutex<Inner>>,
    events: broadcast::Sender<Arc<String>>,
}

struct Inner {
    state: ConnState,
    client: Option<Client>,
    /// Bumped on each connect so a stale connection's events are ignored.
    generation: u64,
    history: VecDeque<Telegram>,
    devices: BTreeMap<IndividualAddress, DeviceStats>,
    health: Health,
    next_id: u64,
    /// The project as used: the ETS export with overrides applied.
    project: Arc<GroupAddressTable>,
    /// The ETS export as loaded.
    base_project: GroupAddressTable,
    /// DPT overrides by group address, and where they are saved.
    overrides: BTreeMap<GroupAddress, DptId>,
    overrides_file: Option<PathBuf>,
    project_file: Option<String>,
}

impl Bus {
    /// Disconnected state with a default gateway and a project.
    pub fn new(
        gateway: Option<String>,
        transport: Transport,
        project: GroupAddressTable,
        file: Option<String>,
    ) -> Self {
        Self::with_health(gateway, transport, project, file, HealthConfig::default())
    }

    /// Like [`Bus::new`] with health thresholds. Must be called inside a Tokio runtime: it
    /// starts the once-a-second health check.
    pub fn with_health(
        gateway: Option<String>,
        transport: Transport,
        project: GroupAddressTable,
        file: Option<String>,
        health: HealthConfig,
    ) -> Self {
        let (events, _) = broadcast::channel(1024);
        let state = ConnState {
            status: Status::Disconnected,
            gateway,
            transport,
            address: None,
            error: None,
        };
        let inner = Inner {
            state,
            client: None,
            generation: 0,
            history: VecDeque::with_capacity(HISTORY),
            devices: BTreeMap::new(),
            health: Health::new(health),
            next_id: 1,
            project: Arc::new(project.clone()),
            base_project: project,
            overrides: BTreeMap::new(),
            overrides_file: None,
            project_file: file,
        };
        let bus = Self {
            inner: Arc::new(Mutex::new(inner)),
            events,
        };
        let ticker = bus.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                ticker.health_tick();
            }
        });
        bus
    }

    /// Run time-based checks and send the issue list if it changed.
    fn health_tick(&self) {
        let issues = {
            let mut inner = self.lock();
            let project = Arc::clone(&inner.project);
            inner.health.tick(&project, now_ms());
            if !inner.health.dirty {
                return;
            }
            inner.health.dirty = false;
            inner.health.issues()
        };
        self.broadcast(&ServerMsg::Health { issues });
    }

    /// Hide an issue until it happens again.
    pub fn dismiss_issue(&self, key: &str) {
        self.lock().health.dismiss(key);
        self.health_tick();
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Subscribe to serialized server messages (JSON).
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<String>> {
        self.events.subscribe()
    }

    /// Send a reply to every tab; tabs match it by request id.
    pub fn reply(&self, msg: ServerMsg) {
        self.broadcast(&msg);
    }

    fn broadcast(&self, msg: &ServerMsg) {
        match serde_json::to_string(msg) {
            Ok(json) => {
                let _ = self.events.send(Arc::new(json));
            }
            Err(err) => tracing::error!(%err, "could not serialize message"),
        }
    }

    /// Everything a newly opened tab needs.
    pub fn hello(&self) -> ServerMsg {
        let inner = self.lock();
        ServerMsg::Hello {
            version: env!("CARGO_PKG_VERSION"),
            state: inner.state.clone(),
            project: project_info(
                &inner.base_project,
                &inner.overrides,
                &inner.project,
                inner.project_file.clone(),
            ),
            history: inner.history.iter().cloned().collect(),
            devices: inner.devices.iter().map(|(a, d)| d.summary(*a)).collect(),
            issues: inner.health.issues(),
        }
    }

    fn set_state(&self, change: impl FnOnce(&mut ConnState)) {
        let state = {
            let mut inner = self.lock();
            change(&mut inner.state);
            inner.state.clone()
        };
        self.broadcast(&ServerMsg::State { state });
    }

    /// Connect to `gateway`, replacing any current connection.
    pub async fn connect(&self, gateway: &str, transport: Transport) -> Result<(), String> {
        let address = parse_endpoint(gateway)?;
        self.disconnect().await;
        let generation = {
            let mut inner = self.lock();
            inner.generation += 1;
            inner.generation
        };
        self.set_state(|s| {
            s.status = Status::Connecting;
            s.gateway = Some(gateway.trim().to_string());
            s.transport = transport;
            s.address = None;
            s.error = None;
        });

        let mut options = TunnelOptions {
            local_echo: true,
            ..TunnelOptions::default()
        };
        if transport == Transport::Tcp {
            options.tunnel.transport = TunnelTransport::Tcp;
        }
        let client = match Client::connect(address, options).await {
            Ok(client) => client,
            Err(err) => {
                let message = err.to_string();
                if self.lock().generation == generation {
                    self.set_state(|s| {
                        s.status = Status::Disconnected;
                        s.error = Some(message.clone());
                    });
                }
                return Err(message);
            }
        };
        let mut events = client.subscribe();
        let tunnel_address = client.address();
        let superseded = {
            let mut inner = self.lock();
            if inner.generation == generation {
                inner.client = Some(client.clone());
                false
            } else {
                true
            }
        };
        if superseded {
            client.close().await;
            return Err("connection was replaced".into());
        }
        self.set_state(|s| {
            s.status = Status::Connected;
            s.address = tunnel_address.map(|a| a.to_string());
        });

        let bus = self.clone();
        tokio::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(n, "bus events dropped");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if bus.lock().generation != generation {
                    break;
                }
                match event {
                    ClientEvent::Frame(ldata) => bus.record(&ldata),
                    ClientEvent::Connected { address } => bus.set_state(|s| {
                        s.status = Status::Connected;
                        s.address = Some(address.to_string());
                        s.error = None;
                    }),
                    ClientEvent::Disconnected { reason } => {
                        bus.lock()
                            .health
                            .disconnected(&reason.to_string(), now_ms());
                        bus.set_state(|s| {
                            s.status = Status::Reconnecting;
                            s.address = None;
                            s.error = Some(reason.to_string());
                        });
                    }
                    ClientEvent::Other(_) => {}
                    ClientEvent::Closed => break,
                }
            }
        });
        Ok(())
    }

    /// Close the connection, if any.
    pub async fn disconnect(&self) {
        let client = {
            let mut inner = self.lock();
            inner.generation += 1;
            inner.client.take()
        };
        if let Some(client) = client {
            client.close().await;
        }
        let was_active = self.lock().state.status != Status::Disconnected;
        if was_active {
            self.set_state(|s| {
                s.status = Status::Disconnected;
                s.address = None;
                s.error = None;
            });
        }
    }

    fn client(&self) -> Result<Client, String> {
        self.lock()
            .client
            .clone()
            .ok_or_else(|| "not connected to the bus".to_string())
    }

    /// Write `value` to a group address. The DPT comes from `dpt` or the project; without one,
    /// `value` is raw (`0`–`63` or `0x…`).
    pub async fn write(
        &self,
        address: &str,
        value: &str,
        dpt: Option<&str>,
    ) -> Result<String, String> {
        let ga: GroupAddress = address
            .trim()
            .parse()
            .map_err(|_| format!("{address:?} is not a group address"))?;
        let dpt = match dpt.map(str::trim).filter(|d| !d.is_empty()) {
            Some(text) => Some(
                text.parse::<DptId>()
                    .map_err(|_| format!("{text:?} is not a DPT"))?,
            ),
            None => self.lock().project.dpt(ga),
        };
        let (payload, shown) = match dpt {
            Some(dpt) => {
                let value = dpt
                    .parse_value(value)
                    .map_err(|e| format!("{value:?} is not a valid {dpt} value: {e}"))?;
                let payload = dpt
                    .encode(&value)
                    .map_err(|e| format!("{value} is not a valid {dpt} value: {e}"))?;
                (payload, dpt.format(&value).to_string())
            }
            None => {
                let payload = Payload::parse_raw(value).ok_or_else(|| {
                    "no DPT known: enter 0–63 or hex like 0x0c1a, or pick a DPT".to_string()
                })?;
                let shown = payload.to_string();
                (payload, shown)
            }
        };
        if let Err(err) = self.client()?.group_write(ga, payload).await {
            if matches!(
                err,
                knxide_tokio::Error::Send(
                    SendError::NegativeConfirmation | SendError::ConfirmationTimeout
                )
            ) {
                self.lock()
                    .health
                    .send_failed(ga, &err.to_string(), now_ms());
            }
            return Err(err.to_string());
        }
        Ok(format!("Wrote {shown} to {ga}"))
    }

    /// Read a group address and describe the answer.
    pub async fn read(&self, address: &str) -> Result<String, String> {
        let ga: GroupAddress = address
            .trim()
            .parse()
            .map_err(|_| format!("{address:?} is not a group address"))?;
        let (source, payload) = self
            .client()?
            .group_read_from(ga, READ_TIMEOUT)
            .await
            .map_err(|e| match e {
                knxide_tokio::Error::Timeout => format!("no response from {ga}"),
                other => other.to_string(),
            })?;
        let dpt = self.lock().project.dpt(ga);
        let value = decode(dpt, &payload).unwrap_or_else(|| payload.to_string());
        Ok(format!("{ga} = {value} (from {source})"))
    }

    /// Restart a device (basic `A_Restart`, keeps its configuration).
    pub async fn restart(&self, address: &str) -> Result<String, String> {
        let device: IndividualAddress = address
            .trim()
            .parse()
            .map_err(|_| format!("{address:?} is not a physical address like 1.1.20"))?;
        self.client()?
            .restart_device(device)
            .await
            .map_err(|e| e.to_string())?;
        Ok(format!("Restart sent to {device}"))
    }

    /// Replace the project.
    pub fn load_project(&self, name: &str, content: &str) -> Result<String, String> {
        let table = GroupAddressTable::parse(content).map_err(|e| e.to_string())?;
        let count = table.len();
        {
            let mut inner = self.lock();
            inner.base_project = table;
            inner.project_file = Some(name.to_string());
        }
        self.project_changed(None);
        Ok(format!("Loaded {count} group addresses from {name}"))
    }

    /// Load DPT overrides from `path` (if it exists) and save future changes there.
    pub fn use_overrides_file(&self, path: PathBuf) -> Result<usize, String> {
        let overrides = match std::fs::read_to_string(&path) {
            Ok(text) => parse_overrides(&text).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };
        let count = overrides.len();
        {
            let mut inner = self.lock();
            inner.overrides = overrides;
            inner.overrides_file = Some(path);
        }
        self.project_changed(None);
        Ok(count)
    }

    /// Override the DPT of `address`, or return it to the project's DPT with `None`.
    pub fn set_dpt(&self, address: &str, dpt: Option<&str>) -> Result<String, String> {
        let ga: GroupAddress = address
            .trim()
            .parse()
            .map_err(|_| format!("{address:?} is not a group address"))?;
        let dpt = match dpt.map(str::trim).filter(|d| !d.is_empty()) {
            Some(text) => {
                let dpt: DptId = text.parse().map_err(|_| format!("{text:?} is not a DPT"))?;
                if !dpt.is_supported() {
                    return Err(format!("DPT {dpt} is not supported"));
                }
                Some(dpt)
            }
            None => None,
        };
        let (file, saved) = {
            let mut inner = self.lock();
            match dpt {
                Some(dpt) => inner.overrides.insert(ga, dpt),
                None => inner.overrides.remove(&ga),
            };
            let file = inner.overrides_file.clone();
            (
                file.clone(),
                file.map(|f| save_overrides(&f, &inner.overrides)),
            )
        };
        self.project_changed(Some(ga));
        if let (Some(file), Some(Err(err))) = (file, saved) {
            return Err(format!(
                "applied, but could not save to {}: {err}",
                file.display()
            ));
        }
        let project_dpt = self.lock().base_project.dpt(ga);
        Ok(match (dpt, project_dpt) {
            (Some(dpt), _) => format!("{ga} now decodes as DPT {dpt}"),
            (None, Some(project)) => format!("{ga} uses the project's DPT {project} again"),
            (None, None) => format!("{ga} has no DPT again"),
        })
    }

    /// Rebuild the effective project and tell every tab. `changed` is the one address whose
    /// DPT changed, or `None` when the whole project may have.
    fn project_changed(&self, changed: Option<GroupAddress>) {
        let info = {
            let mut inner = self.lock();
            let effective = apply_overrides(&inner.base_project, &inner.overrides);
            inner.project = Arc::new(effective);
            match changed {
                Some(ga) => inner.health.address_changed(ga),
                None => inner.health.project_changed(),
            }
            project_info(
                &inner.base_project,
                &inner.overrides,
                &inner.project,
                inner.project_file.clone(),
            )
        };
        self.broadcast(&ServerMsg::Project { project: info });
        self.health_tick();
    }

    fn record(&self, ldata: &LData) {
        let telegram = {
            let mut inner = self.lock();
            let id = inner.next_id;
            inner.next_id += 1;
            let own_address: Option<IndividualAddress> =
                inner.state.address.as_deref().and_then(|a| a.parse().ok());
            let own = own_address == Some(ldata.source);
            let telegram = telegram(id, ldata, &inner.project, own);
            let project = Arc::clone(&inner.project);
            inner
                .health
                .observe(ldata, &project, own_address, telegram.time);
            if inner.history.len() >= HISTORY {
                inner.history.pop_front();
            }
            inner.history.push_back(telegram.clone());
            let stats = inner
                .devices
                .entry(ldata.source)
                .or_insert_with(|| DeviceStats {
                    frames: 0,
                    first_seen: telegram.time,
                    last_seen: telegram.time,
                    groups: BTreeMap::new(),
                });
            stats.frames += 1;
            stats.last_seen = telegram.time;
            if let Some(ga) = ldata.group_address() {
                *stats.groups.entry(ga).or_default() += 1;
            }
            telegram
        };
        self.broadcast(&ServerMsg::Telegram { telegram });
    }
}

struct DeviceStats {
    frames: u64,
    first_seen: u64,
    last_seen: u64,
    groups: BTreeMap<GroupAddress, u64>,
}

impl DeviceStats {
    fn summary(&self, address: IndividualAddress) -> Device {
        Device {
            address: address.to_string(),
            frames: self.frames,
            first_seen: self.first_seen,
            last_seen: self.last_seen,
            groups: self
                .groups
                .iter()
                .map(|(ga, n)| (ga.to_string(), *n))
                .collect(),
        }
    }
}

fn parse_endpoint(text: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("enter the gateway's IP address".into());
    }
    text.parse::<SocketAddr>()
        .or_else(|_| {
            text.parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, knxide_ip::frame::KNXNETIP_PORT))
        })
        .map_err(|_| format!("{text:?} is not an IP address"))
}

fn decode(dpt: Option<DptId>, payload: &Payload) -> Option<String> {
    let dpt = dpt?;
    dpt.decode(payload).ok().map(|v| dpt.format(&v).to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn telegram(id: u64, ldata: &LData, project: &GroupAddressTable, own: bool) -> Telegram {
    let ga = ldata.group_address();
    let entry = ga.and_then(|ga| project.get(ga));
    let dpt = entry.and_then(|e| e.dpt).filter(|d| d.is_supported());
    let (service, payload) = match ldata.apdu() {
        Some(Apdu::GroupValueWrite(p)) => ("write", Some(p)),
        Some(Apdu::GroupValueResponse(p)) => ("response", Some(p)),
        Some(Apdu::GroupValueRead) => ("read", None),
        _ => ("other", None),
    };
    let raw = match (payload, &ldata.tpdu) {
        (Some(p), _) => Some(p.to_string()),
        (None, Tpdu::Data(Apdu::GroupValueRead)) => None,
        (None, tpdu) => Some(describe_tpdu(tpdu)),
    };
    Telegram {
        id,
        time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64),
        source: ldata.source.to_string(),
        destination: ldata.destination.to_string(),
        group: ga.is_some(),
        service,
        raw,
        value: payload.and_then(|p| decode(dpt, p)),
        dpt: dpt.map(|d| d.to_string()),
        name: entry.map(|e| e.name.clone()).filter(|n| !n.is_empty()),
        own,
        warning: payload.and_then(|p| value_warning(dpt, p)),
    }
}

fn describe_tpdu(tpdu: &Tpdu) -> String {
    match tpdu {
        Tpdu::Connect => "T_Connect".into(),
        Tpdu::Disconnect => "T_Disconnect".into(),
        Tpdu::Ack { sequence } => format!("T_Ack #{sequence}"),
        Tpdu::Nak { sequence } => format!("T_Nak #{sequence}"),
        other => match other.apdu() {
            Some(Apdu::Other { apci, data }) => {
                let data = Payload::Bytes(data.clone());
                format!("APCI {apci:#05x} {data}").trim_end().to_string()
            }
            _ => format!("{other:?}"),
        },
    }
}

fn apply_overrides(
    base: &GroupAddressTable,
    overrides: &BTreeMap<GroupAddress, DptId>,
) -> GroupAddressTable {
    let mut table = base.clone();
    for (&address, &dpt) in overrides {
        let mut entry = base.get(address).cloned().unwrap_or(GroupAddressInfo {
            address,
            name: String::new(),
            description: String::new(),
            dpt: None,
            path: Vec::new(),
        });
        entry.dpt = Some(dpt);
        table.insert(entry);
    }
    table
}

/// `{"5/0/1": "14"}`
fn parse_overrides(text: &str) -> Result<BTreeMap<GroupAddress, DptId>, String> {
    let raw: BTreeMap<String, String> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    raw.into_iter()
        .map(|(ga, dpt)| {
            let ga = ga
                .parse()
                .map_err(|_| format!("{ga:?} is not a group address"))?;
            let dpt = dpt.parse().map_err(|_| format!("{dpt:?} is not a DPT"))?;
            Ok((ga, dpt))
        })
        .collect()
}

fn save_overrides(
    path: &std::path::Path,
    overrides: &BTreeMap<GroupAddress, DptId>,
) -> std::io::Result<()> {
    let raw: BTreeMap<String, String> = overrides
        .iter()
        .map(|(ga, dpt)| (ga.to_string(), dpt.to_string()))
        .collect();
    let json = serde_json::to_string_pretty(&raw).map_err(std::io::Error::other)?;
    std::fs::write(path, json + "\n")
}

fn project_info(
    base: &GroupAddressTable,
    overrides: &BTreeMap<GroupAddress, DptId>,
    table: &GroupAddressTable,
    file: Option<String>,
) -> ProjectInfo {
    let entries = table
        .iter()
        .map(|e| GaEntry {
            address: e.address.to_string(),
            name: e.name.clone(),
            description: e.description.clone(),
            path: e.path.clone(),
            dpt: e.dpt.map(|d| d.to_string()),
            project_dpt: overrides
                .contains_key(&e.address)
                .then(|| base.dpt(e.address).map(|d| d.to_string()))
                .flatten(),
            overridden: overrides.contains_key(&e.address),
            unit: e.dpt.and_then(DptId::unit),
            kind: e.dpt.and_then(DptId::value_kind).map(Kind::from_value_kind),
        })
        .collect();
    ProjectInfo { file, entries }
}
