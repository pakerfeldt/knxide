//! Server state: the bus connection, live values, the project and the dashboard layout.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use knxide_core::dpt::{DptId, DptValue};
use knxide_core::{Apdu, Cemi, GroupAddress, LData, Payload};
use knxide_project::GroupAddressTable;
use knxide_tokio::{Client, ClientEvent, TunnelOptions};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::config::{Dashboard, Tile, ranges, suggest};

/// The latest value of one group address, as the page shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Value {
    /// Formatted with unit and labels: `21.5 °C`, `on`.
    pub text: String,
    /// For controls: a boolean or number when the DPT has one, else the text.
    pub value: serde_json::Value,
    /// Milliseconds since the Unix epoch.
    pub time: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub address: String,
    pub name: String,
    pub path: Vec<String>,
    pub dpt: Option<String>,
    pub unit: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Connecting,
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnState {
    pub status: Status,
    pub gateway: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello {
        dashboard: Dashboard,
        project: Vec<Entry>,
        ranges: Vec<Vec<String>>,
        values: BTreeMap<String, Value>,
        state: ConnState,
    },
    State {
        state: ConnState,
    },
    Value {
        ga: String,
        value: Value,
    },
    Dashboard {
        dashboard: Dashboard,
    },
    Result {
        id: u64,
        ok: bool,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tiles: Option<Vec<Tile>>,
    },
}

struct Inner {
    state: ConnState,
    client: Option<Client>,
    values: HashMap<GroupAddress, Value>,
    project: Arc<GroupAddressTable>,
    dashboard: Dashboard,
    dashboard_file: Option<PathBuf>,
}

/// Shared server state.
#[derive(Clone)]
pub struct App {
    inner: Arc<Mutex<Inner>>,
    events: broadcast::Sender<Arc<String>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl App {
    /// State for `project` and `dashboard`; saves go to `dashboard_file`.
    pub fn new(
        gateway: String,
        project: GroupAddressTable,
        dashboard: Dashboard,
        dashboard_file: Option<PathBuf>,
    ) -> Self {
        let (events, _) = broadcast::channel(1024);
        let inner = Inner {
            state: ConnState {
                status: Status::Connecting,
                gateway,
                error: None,
            },
            client: None,
            values: HashMap::new(),
            project: Arc::new(project),
            dashboard,
            dashboard_file,
        };
        Self {
            inner: Arc::new(Mutex::new(inner)),
            events,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<String>> {
        self.events.subscribe()
    }

    pub fn broadcast(&self, msg: &ServerMsg) {
        if let Ok(json) = serde_json::to_string(msg) {
            let _ = self.events.send(Arc::new(json));
        }
    }

    pub fn hello(&self) -> ServerMsg {
        let inner = self.lock();
        ServerMsg::Hello {
            dashboard: inner.dashboard.clone(),
            project: inner
                .project
                .iter()
                .map(|e| Entry {
                    address: e.address.to_string(),
                    name: e.name.clone(),
                    path: e.path.clone(),
                    dpt: e.dpt.map(|d| d.to_string()),
                    unit: e.dpt.and_then(DptId::unit),
                })
                .collect(),
            ranges: ranges(&inner.project),
            values: inner
                .values
                .iter()
                .map(|(ga, v)| (ga.to_string(), v.clone()))
                .collect(),
            state: inner.state.clone(),
        }
    }

    fn set_state(&self, status: Status, error: Option<String>) {
        let state = {
            let mut inner = self.lock();
            inner.state.status = status;
            inner.state.error = error;
            inner.state.clone()
        };
        self.broadcast(&ServerMsg::State { state });
    }

    /// Keep a connection to the gateway open forever.
    pub async fn run_bus(self, gateway: SocketAddr, options: TunnelOptions) {
        loop {
            let client = match Client::connect(gateway, options.clone()).await {
                Ok(client) => client,
                Err(err) => {
                    self.set_state(Status::Disconnected, Some(err.to_string()));
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            let mut events = client.subscribe();
            self.lock().client = Some(client.clone());
            self.set_state(Status::Connected, None);
            tokio::spawn(self.clone().read_all());
            loop {
                match events.recv().await {
                    Ok(ClientEvent::Frame(ldata)) => self.record(&ldata),
                    Ok(ClientEvent::Connected { .. }) => {
                        self.set_state(Status::Connected, None);
                        tokio::spawn(self.clone().read_all());
                    }
                    Ok(ClientEvent::Disconnected { reason }) => {
                        self.set_state(Status::Connecting, Some(reason.to_string()));
                    }
                    Ok(ClientEvent::Other(_)) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Ok(ClientEvent::Closed) | Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            self.lock().client = None;
        }
    }

    /// Ask the bus for every value the dashboard shows. Answers arrive as normal traffic.
    pub async fn read_all(self) {
        let (client, addresses) = {
            let inner = self.lock();
            (inner.client.clone(), inner.dashboard.shown_addresses())
        };
        let Some(client) = client else { return };
        for ga in addresses {
            if client
                .send(Cemi::LDataReq(LData::group(ga, Apdu::GroupValueRead)))
                .await
                .is_err()
            {
                return;
            }
        }
    }

    fn record(&self, ldata: &LData) {
        let Some(ga) = ldata.group_address() else {
            return;
        };
        let (Some(Apdu::GroupValueWrite(payload)) | Some(Apdu::GroupValueResponse(payload))) =
            ldata.apdu()
        else {
            return;
        };
        let value = {
            let mut inner = self.lock();
            let value = decode(inner.project.dpt(ga), payload);
            inner.values.insert(ga, value.clone());
            value
        };
        self.broadcast(&ServerMsg::Value {
            ga: ga.to_string(),
            value,
        });
    }

    /// Write `value` (text, as typed or as the page's controls produce it). The DPT is the
    /// project's, else `hint`; without either, `value` is raw (`0`–`63` or `0x…`).
    pub async fn write(&self, ga: &str, value: &str, hint: Option<&str>) -> Result<String, String> {
        let ga: GroupAddress = ga
            .trim()
            .parse()
            .map_err(|_| format!("{ga:?} is not a group address"))?;
        let client = self
            .lock()
            .client
            .clone()
            .ok_or("not connected to the bus")?;
        let dpt = self
            .lock()
            .project
            .dpt(ga)
            .or_else(|| hint.and_then(|h| h.parse().ok()))
            .filter(|d: &DptId| d.is_supported());
        let payload = match dpt {
            Some(dpt) => {
                let v = dpt
                    .parse_value(value)
                    .map_err(|e| format!("{value:?} is not a valid {dpt} value: {e}"))?;
                dpt.encode(&v)
                    .map_err(|e| format!("{value:?} is not a valid {dpt} value: {e}"))?
            }
            None => Payload::parse_raw(value)
                .ok_or_else(|| format!("{ga} has no DPT; {value:?} is not a raw value"))?,
        };
        client
            .group_write(ga, payload)
            .await
            .map_err(|e| e.to_string())?;
        Ok(format!("{ga} ← {value}"))
    }

    /// Ask the bus for one value.
    pub async fn read(&self, ga: &str) {
        let Ok(ga) = ga.trim().parse::<GroupAddress>() else {
            return;
        };
        let client = self.lock().client.clone();
        if let Some(client) = client {
            let _ = client
                .send(Cemi::LDataReq(LData::group(ga, Apdu::GroupValueRead)))
                .await;
        }
    }

    /// Replace the layout, save it, and show it in every tab.
    pub async fn save(&self, dashboard: Dashboard) -> Result<String, String> {
        dashboard.validate()?;
        let file = self.lock().dashboard_file.clone();
        if let Some(file) = &file {
            let copy = dashboard.clone();
            let path = file.clone();
            tokio::task::spawn_blocking(move || copy.save(&path))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("could not save {}: {e}", file.display()))?;
        }
        self.lock().dashboard = dashboard.clone();
        self.broadcast(&ServerMsg::Dashboard { dashboard });
        tokio::spawn(self.clone().read_all());
        Ok("Saved".into())
    }

    /// Tiles proposed for a group range of the project.
    pub fn suggest(&self, path: &[String]) -> Vec<Tile> {
        suggest(&self.lock().project, path)
    }
}

/// Decode with the DPT, keeping a boolean or number for the page's controls.
fn decode(dpt: Option<DptId>, payload: &Payload) -> Value {
    let time = now_ms();
    let Some(dpt) = dpt else {
        return Value {
            text: payload.to_string(),
            value: serde_json::Value::Null,
            time,
        };
    };
    match dpt.decode(payload) {
        Ok(v) => {
            let value = match &v {
                DptValue::Bool(b) => serde_json::Value::Bool(*b),
                DptValue::Float(f) => serde_json::Number::from_f64(f64::from(*f))
                    .map_or(serde_json::Value::Null, serde_json::Value::Number),
                DptValue::Unsigned(u) => (*u).into(),
                DptValue::Signed(i) => (*i).into(),
                other => other.to_string().into(),
            };
            Value {
                text: dpt.format(&v).to_string(),
                value,
                time,
            }
        }
        Err(_) => Value {
            text: payload.to_string(),
            value: serde_json::Value::Null,
            time,
        },
    }
}

/// Apply `<project>.overrides.json` (written by knxide-monitor) if present.
pub fn apply_overrides(mut table: GroupAddressTable, path: &std::path::Path) -> GroupAddressTable {
    let Ok(text) = std::fs::read_to_string(path) else {
        return table;
    };
    let Ok(raw) = serde_json::from_str::<BTreeMap<String, String>>(&text) else {
        tracing::warn!(path = %path.display(), "ignoring unreadable DPT overrides");
        return table;
    };
    for (ga, dpt) in raw {
        let (Ok(ga), Ok(dpt)) = (ga.parse::<GroupAddress>(), dpt.parse::<DptId>()) else {
            continue;
        };
        if let Some(mut entry) = table.get(ga).cloned() {
            entry.dpt = Some(dpt);
            table.insert(entry);
        }
    }
    table
}

/// `IP` or `IP:PORT` to a socket address on the KNXnet/IP port.
pub fn parse_endpoint(text: &str) -> Result<SocketAddr, String> {
    text.parse::<SocketAddr>()
        .or_else(|_| {
            text.parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, knxide_ip::frame::KNXNETIP_PORT))
        })
        .map_err(|_| format!("invalid address {text:?}"))
}
