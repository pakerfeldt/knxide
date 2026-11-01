//! JSON messages between the browser and the server, over one WebSocket.

use serde::{Deserialize, Serialize};

pub use crate::health::Issue;

/// Server to browser.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Sent once after the socket opens: everything needed to draw the page.
    Hello {
        version: &'static str,
        state: ConnState,
        project: ProjectInfo,
        history: Vec<Telegram>,
        /// Every device heard since the server started.
        devices: Vec<Device>,
        issues: Vec<Issue>,
    },
    /// The health issue list changed.
    Health { issues: Vec<Issue> },
    /// The KNX connection changed.
    State { state: ConnState },
    /// A frame on the bus.
    Telegram { telegram: Telegram },
    /// A new project file was loaded.
    Project { project: ProjectInfo },
    /// Outcome of a browser request.
    Result { id: u64, ok: bool, message: String },
}

/// Browser to server.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Connect {
        gateway: String,
        transport: Transport,
    },
    Disconnect,
    Write {
        id: u64,
        address: String,
        value: String,
        dpt: Option<String>,
    },
    Read {
        id: u64,
        address: String,
    },
    Restart {
        id: u64,
        address: String,
    },
    DismissIssue {
        key: String,
    },
    /// Override the DPT of one group address; `None` returns to the project's DPT.
    SetDpt {
        id: u64,
        address: String,
        dpt: Option<String>,
    },
    LoadProject {
        id: u64,
        name: String,
        content: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// How to reach the gateway.
pub enum Transport {
    /// Tunnelling over UDP.
    #[default]
    Udp,
    /// Tunnelling over TCP.
    Tcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnState {
    pub status: Status,
    /// The gateway we are or were connected to, or the configured default.
    pub gateway: Option<String>,
    pub transport: Transport,
    /// Our tunnel address while connected.
    pub address: Option<String>,
    /// Why the last attempt failed or the connection dropped.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Telegram {
    pub id: u64,
    /// Milliseconds since the Unix epoch.
    pub time: u64,
    pub source: String,
    pub destination: String,
    /// Destination is a group address.
    pub group: bool,
    /// `write`, `read`, `response`, or `other`.
    pub service: &'static str,
    /// Payload in hex, or a short description for non-group frames.
    pub raw: Option<String>,
    /// Decoded and formatted value, when the DPT is known.
    pub value: Option<String>,
    pub dpt: Option<String>,
    /// Group address name from the project.
    pub name: Option<String>,
    /// Sent by this server.
    pub own: bool,
    /// The value does not fit the project's DPT.
    pub warning: Option<String>,
}

/// A physical address heard on the bus. Browsers keep it current from live telegrams.
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub address: String,
    pub frames: u64,
    /// Milliseconds since the Unix epoch.
    pub first_seen: u64,
    pub last_seen: u64,
    /// Group addresses it sent to, with frame counts.
    pub groups: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub file: Option<String>,
    pub entries: Vec<GaEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GaEntry {
    pub address: String,
    pub name: String,
    pub description: String,
    pub path: Vec<String>,
    /// The DPT in use: the override if there is one, else the project's.
    pub dpt: Option<String>,
    /// The DPT from the ETS project, when an override replaces it.
    pub project_dpt: Option<String>,
    pub overridden: bool,
    pub unit: Option<&'static str>,
    /// Which input control fits, see `knxide_core::dpt::ValueKind`.
    pub kind: Option<Kind>,
}

/// Input control hints for the browser.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    Bool { off: &'static str, on: &'static str },
    Control,
    Step,
    Integer { min: i64, max: i64 },
    Float { min: f32, max: f32 },
    Time,
    Date,
    Text { max_len: usize },
    SceneControl,
    HvacMode,
    Rgb,
}

impl Kind {
    pub fn from_value_kind(kind: knxide_core::dpt::ValueKind) -> Self {
        use knxide_core::dpt::ValueKind as V;
        match kind {
            V::Bool { off, on } => Self::Bool { off, on },
            V::Control => Self::Control,
            V::Step => Self::Step,
            V::Integer { min, max } => Self::Integer { min, max },
            V::Float { min, max } => Self::Float { min, max },
            V::TimeOfDay => Self::Time,
            V::Date => Self::Date,
            V::Text { max_len } => Self::Text { max_len },
            V::SceneControl => Self::SceneControl,
            V::HvacMode => Self::HvacMode,
            V::Rgb => Self::Rgb,
        }
    }
}
