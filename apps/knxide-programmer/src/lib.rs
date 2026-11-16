//! The knxide-programmer server: one page, a JSON API under `/api`, and a WebSocket that
//! carries the progress of the running operation.
//!
//! The project is read once at start-up and never changed: it is the baseline. What the user
//! changes (parameter values, group address links, group addresses made or renamed) lives in
//! a file beside the project ([`sidecar`]); a device's effective configuration is the project
//! with those edits on top. One operation runs at a time. The bus is opened for an operation
//! and closed after it, so the tunnel is free for other tools in between.

mod export;
mod floor_plans;
pub mod interfaces;
pub mod link;
pub mod ops;
mod queue;
pub mod session;
pub mod sidecar;
pub mod sim;
mod source;
pub mod startup;
mod view;
pub mod workspace;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::Path;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use knxide_core::{GroupAddress, IndividualAddress};
use knxide_program::compare::{self, Finding};
use knxide_program::edit::{self, Changes, EditError};
use knxide_program::view::ObjectView;
use knxide_program::{
    DeviceData, DeviceView, Download, Edits, PlanOptions, Project, ProjectDevice, SimulatedDevice,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::link::{BusAccess, ClientLink, Link};
use crate::ops::{Cancel, CancelSwitch, CompareLimits, DownloadLimits, Failure, Report};
use crate::session::{Session, SessionConfig};
use crate::sidecar::{CreatedAddress, Downloaded};
use crate::sim::SimBus;
pub use crate::source::{load_project, load_project_in};
use crate::workspace::Open;
pub use crate::workspace::{BusConfig, Workspace};

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_CSS: &str = include_str!("../static/app.css");
const APP_JS: &str = include_str!("../static/app.js");
const MODEL_JS: &str = include_str!("../static/model.js");

/// How this server plans: with its own safeguard on top of what the recorded tool does. A
/// device that holds another application is left to the tool that put it there.
fn plan_options() -> PlanOptions {
    PlanOptions {
        require_same_application: true,
        ..PlanOptions::default()
    }
}

/// Log lines kept for pages that open later.
const LOG_LINES: usize = 20_000;
/// Finished jobs kept for the activity view.
const JOBS_KEPT: usize = 100;

/// How operations reach devices.
#[derive(Debug, Clone)]
pub enum Bus {
    /// Not at all: the page shows and edits, and downloads nothing.
    None,
    /// Over KNXnet/IP.
    Access(BusAccess),
    /// Over KNXnet/IP routing from this individual address, on the interface chosen each
    /// time the bus is used: a machine that gets its cable after the program was started is
    /// on the cable from then on.
    AutoRouting(IndividualAddress),
    /// Simulated devices, with this individual address for the tool.
    Simulated(SimBus, IndividualAddress),
}

impl Bus {
    /// Simulated devices for every supported device of `project`, each holding the
    /// application the project gives it, with empty memory.
    pub fn simulate(project: &Project, tool: IndividualAddress) -> Self {
        Self::simulate_on(SimBus::new(), project, tool)
    }

    /// Like [`Bus::simulate`], on `bus`: one with frames as slow as on a real bus, say.
    pub fn simulate_on(bus: SimBus, project: &Project, tool: IndividualAddress) -> Self {
        for device in &project.devices {
            if let (true, Some(address), Some(download)) =
                (device.is_editable(), device.address, &device.download)
            {
                bus.add(address, knxide_program::SimulatedDevice::holding(download));
            }
        }
        Self::Simulated(bus, tool)
    }

    /// The access over KNXnet/IP as it is now: for automatic routing, on the interface that
    /// is chosen at this moment. `None` without KNXnet/IP, and without a network to route on.
    fn access(&self) -> Option<BusAccess> {
        match self {
            Self::Access(access) => Some(access.clone()),
            Self::AutoRouting(address) => BusAccess::routing_now(*address),
            Self::None | Self::Simulated(..) => None,
        }
    }

    fn describe(&self) -> Value {
        match self {
            Self::None => json!({ "mode": "none", "text": "No bus" }),
            Self::Simulated(_, address) => json!({
                "mode": "simulated",
                "text": format!("Simulated devices (tool address {address})"),
            }),
            Self::Access(_) | Self::AutoRouting(_) => match self.access() {
                Some(access) => Self::describe_access(&access),
                None => json!({
                    "mode": "routing",
                    "text": "routing: no network",
                    "interface": Value::Null,
                    "wireless": false,
                    "warning": link::NO_NETWORK_WARNING,
                }),
            },
        }
    }

    fn describe_access(access: &BusAccess) -> Value {
        json!({
            "mode": match access {
                BusAccess::Tunnel { .. } => "tunnel",
                BusAccess::Routing { .. } => "routing",
            },
            "text": access.to_string(),
            "interface": match access {
                BusAccess::Routing { name, interface, kind, .. } => json!({
                    "name": name,
                    "address": interface.to_string(),
                    "kind": match kind {
                        interfaces::Kind::Wired => "wired",
                        interfaces::Kind::Wireless => "wireless",
                        interfaces::Kind::Unknown => "unknown",
                    },
                }),
                BusAccess::Tunnel { .. } => Value::Null,
            },
            "wireless": access.is_wireless(),
            "warning": access.is_wireless().then_some(link::WIRELESS_WARNING),
        })
    }
}

/// Timers, limits and where edits are kept.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// Transport timers and retries.
    pub session: SessionConfig,
    /// How persistent a comparison is.
    pub limits: CompareLimits,
    /// How a download recovers from an exchange that fails.
    pub download: DownloadLimits,
    /// The file edits are kept in. `None` keeps them in memory only, for tests.
    pub state: Option<PathBuf>,
}

/// What an operation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    /// Read the device and compare it with the plan. Nothing is written.
    Compare,
    /// Download the application.
    Download,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum JobStatus {
    Running,
    Succeeded,
    /// A comparison that had to give up: its result covers part of the memory.
    Incomplete,
    Failed,
    Cancelled,
}

/// One operation, as the page sees it.
#[derive(Debug, Clone, Serialize)]
struct Job {
    id: u64,
    kind: JobKind,
    device: String,
    device_name: String,
    address: String,
    status: JobStatus,
    started_ms: u64,
    finished_ms: Option<u64>,
    /// Requests sent so far.
    sent: usize,
    /// Requests the operation expects to send in all. For a download: what the plan against a
    /// modelled device has. For a comparison: the pre-check and the reads it plans, corrected
    /// as it goes (reads added, connections opened anew). When the job has succeeded it is
    /// what was sent.
    expected: usize,
    /// The parts the operation goes through, in words, and the one it is in.
    phases: Vec<&'static str>,
    phase: Option<&'static str>,
    /// Frames that had to be sent again, answers asked for again on the same connection
    /// because the first was lost (comparisons only), and connections opened anew: how bad
    /// the link is.
    repetitions: u32,
    re_requests: u32,
    reconnects: u32,
    /// For a download: exchanges that failed and were recovered from by reading what the
    /// device holds.
    recoveries: u32,
    /// Why it stopped, and which check stopped it.
    error: Option<String>,
    check: Option<&'static str>,
    /// For an operation that stopped: what the device has been told by then, in a sentence.
    written: Option<String>,
    /// For a download that stopped: the steps completed, the step under way, the writes the
    /// device took.
    progress: Option<Value>,
    /// What came out of it.
    result: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct LogLine {
    seq: u64,
    time_ms: u64,
    job: u64,
    /// `request`, `response`, `note`, `info` or `error`.
    level: &'static str,
    text: String,
}

/// A device's configuration with the edits applied, and what follows from it.
struct Effective {
    data: DeviceData,
    view: Result<DeviceView, String>,
    download: Result<Download, String>,
    /// What differs from the baseline: from what the device was last programmed with, or
    /// from the project where the app has never programmed it.
    changes: Changes,
    /// How many parameters and group objects that is.
    pending: usize,
    /// What differs from the project file. Secondary: worth knowing, nothing to act on.
    from_project: Changes,
    /// Linked objects the edits switched off, and objects they brought in.
    dropped: Vec<ObjectView>,
    gained: Vec<ObjectView>,
    /// The page each parameter is on, now or, for one the edits hid, in the project.
    pages: view::PageTitles,
    digest: Option<String>,
}

#[derive(Default)]
struct Shared {
    job: Option<Job>,
    switch: Option<CancelSwitch>,
    log: VecDeque<LogLine>,
    next_job: u64,
    next_line: u64,
    /// Finished jobs, oldest first.
    jobs: Vec<Job>,
    /// The last finished operation per device.
    outcomes: HashMap<String, Job>,
    /// What the last comparison of each device found.
    findings: HashMap<String, Vec<Finding>>,
    /// The edits.
    state: sidecar::State,
    /// Effective configurations by device, as long as the device's edits stay the same.
    cache: HashMap<String, Arc<Effective>>,
    /// The devices being programmed one after another, or the last such run.
    queue: Option<queue::Queue>,
    next_queue: u64,
    /// The images of floor plans, where there is no state file to keep them beside.
    plan_images: HashMap<String, Vec<u8>>,
}

struct Inner {
    project: Project,
    source: String,
    bus: Bus,
    settings: Settings,
    events: broadcast::Sender<Arc<String>>,
    shared: Mutex<Shared>,
}

/// The server's state: the project, the edits, the bus access and the running operation.
#[derive(Clone)]
pub struct Programmer {
    inner: Arc<Inner>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// A request the API turns down.
struct Refusal(StatusCode, String);

impl Refusal {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self(status, message.into())
    }

    fn missing() -> Self {
        Self::new(StatusCode::NOT_FOUND, "no such device")
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        json_response(self.0, &json!({ "error": self.1 }))
    }
}

impl From<EditError> for Refusal {
    fn from(error: EditError) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, error.to_string())
    }
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body.to_string(),
    )
        .into_response()
}

impl Programmer {
    /// A server for `project`, which was read from `source` (shown on the page), with
    /// operations going through `bus`. Edits are kept in memory only.
    pub fn new(project: Project, source: String, bus: Bus) -> Self {
        Self::with(
            project,
            source,
            bus,
            Settings::default(),
            sidecar::State::default(),
        )
    }

    /// Like [`Programmer::new`], with other timers and limits; tests shorten them.
    pub fn with_timing(
        project: Project,
        source: String,
        bus: Bus,
        session: SessionConfig,
        limits: CompareLimits,
    ) -> Self {
        let settings = Settings {
            session,
            limits,
            // In step with the other timers.
            download: DownloadLimits {
                lost_answer_after: limits.lost_answer_after,
                unanswered_after: Some(session.transport.response_timeout / 2),
                reconnect_pause: limits.reconnect_pause,
                ..DownloadLimits::default()
            },
            state: None,
        };
        Self::with(project, source, bus, settings, sidecar::State::default())
    }

    /// A server that keeps its edits in the file `settings.state` names, and starts with what
    /// that file holds. Fails when the file is there and cannot be used.
    pub fn open(
        project: Project,
        source: String,
        bus: Bus,
        settings: Settings,
    ) -> Result<Self, String> {
        let state = match &settings.state {
            Some(path) => sidecar::State::load(path, &project.id)?,
            None => sidecar::State::default(),
        };
        let programmer = Self::with(project, source, bus, settings, state);
        // Records from before configurations were kept are completed where the configuration
        // here is the one they name.
        let old: Vec<String> = programmer
            .shared()
            .state
            .devices
            .iter()
            .filter(|(_, state)| {
                state
                    .downloaded
                    .as_ref()
                    .is_some_and(|record| record.edits.is_none())
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in old {
            if let Some(device) = programmer.inner.project.device(&id) {
                programmer.standing(device);
            }
        }
        Ok(programmer)
    }

    fn with(
        project: Project,
        source: String,
        bus: Bus,
        settings: Settings,
        state: sidecar::State,
    ) -> Self {
        let (events, _) = broadcast::channel(4096);
        Self {
            inner: Arc::new(Inner {
                project,
                source,
                bus,
                settings,
                events,
                shared: Mutex::new(Shared {
                    next_job: 1,
                    state,
                    ..Shared::default()
                }),
            }),
        }
    }

    /// The project the server works from.
    pub fn project(&self) -> &Project {
        &self.inner.project
    }

    /// Whether an operation is running.
    pub fn running(&self) -> bool {
        self.shared()
            .job
            .as_ref()
            .is_some_and(|job| job.status == JobStatus::Running)
    }

    /// The edits as they are now.
    pub(crate) fn state_copy(&self) -> sidecar::State {
        self.shared().state.clone()
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn broadcast(&self, message: &Value) {
        let _ = self.inner.events.send(Arc::new(message.to_string()));
    }

    fn log(&self, job: u64, level: &'static str, text: String) {
        let line = {
            let mut shared = self.shared();
            let line = LogLine {
                seq: shared.next_line,
                time_ms: now_ms(),
                job,
                level,
                text,
            };
            shared.next_line += 1;
            if shared.log.len() >= LOG_LINES {
                shared.log.pop_front();
            }
            shared.log.push_back(line.clone());
            line
        };
        self.broadcast(&json!({ "type": "log", "line": line }));
    }

    fn device(&self, id: &str) -> Result<&ProjectDevice, Refusal> {
        self.inner.project.device(id).ok_or_else(Refusal::missing)
    }

    // ------------------------------------------------------------ effective configuration

    /// The device's configuration with `edits` applied, and what of it differs from the
    /// configuration `base` leads to: the baseline, as edits over the project like `edits`.
    fn compute(&self, device: &ProjectDevice, edits: &Edits, base: &Edits) -> Option<Effective> {
        let program = device.application.as_deref()?;
        let project = &self.inner.project;
        let (data, addresses) = edits.apply(program, &device.data, &project.addresses);
        let (base_data, base_addresses) = base.apply(program, &device.data, &project.addresses);
        let view = knxide_program::view(program, &base_data, &data, &addresses)
            .map_err(|error| error.to_string());
        let download =
            Download::prepare(program, &data, &addresses).map_err(|error| error.to_string());
        let relative = edits
            .relative_to(program, &device.data, &project.addresses, base)
            .unwrap_or_default();
        let changes =
            edit::changes(program, &base_data, &base_addresses, &relative).unwrap_or_default();
        let from_project =
            edit::changes(program, &device.data, &project.addresses, edits).unwrap_or_default();
        let digest = download.as_ref().ok().map(sidecar::digest);
        // What the parameter changes do to the group objects: worked out against the
        // baseline, and only when a parameter was changed.
        let (mut dropped, mut gained) = (Vec::new(), Vec::new());
        let mut pages = view::PageTitles::new();
        if let Ok(now) = &view
            && !relative.parameters.is_empty()
            && let Ok(before) =
                knxide_program::view(program, &base_data, &base_data, &base_addresses)
        {
            pages = view::page_titles(&before);
            let was_in_use = |object: &&ObjectView| {
                before
                    .objects
                    .iter()
                    .any(|other| other.reference == object.reference)
            };
            dropped = now.unused.iter().filter(was_in_use).cloned().collect();
            gained = now
                .objects
                .iter()
                .filter(|object| !was_in_use(object))
                .cloned()
                .collect();
        }
        if let Ok(now) = &view {
            pages.extend(view::page_titles(now));
        }
        Some(Effective {
            data,
            view,
            download,
            changes,
            pending: relative.parameters.len() + relative.links.len(),
            from_project,
            dropped,
            gained,
            pages,
            digest,
        })
    }

    /// The baseline to measure the edits `state` against: what the device was last programmed
    /// with. Where the app has never programmed it, the project (no edits). Where a record
    /// from before configurations were kept says only that it was programmed, the edits as
    /// they are: nothing can be told apart then, and [`Self::standing`] says the rest.
    fn base_of(state: Option<&sidecar::DeviceState>) -> Edits {
        match state {
            Some(state) => match &state.downloaded {
                Some(record) => record.edits.clone().unwrap_or_else(|| state.edits.clone()),
                None => Edits::default(),
            },
            None => Edits::default(),
        }
    }

    /// The edits that take the device back to its baseline: what Discard and Undo go to.
    fn baseline(&self, id: &str) -> Edits {
        Self::base_of(self.shared().state.devices.get(id))
    }

    /// The device's effective configuration; `None` for a device without application program.
    fn effective(&self, device: &ProjectDevice) -> Option<Arc<Effective>> {
        let (edits, base) = {
            let shared = self.shared();
            if let Some(cached) = shared.cache.get(&device.id) {
                return Some(Arc::clone(cached));
            }
            let state = shared.state.devices.get(&device.id);
            (
                state.map(|state| state.edits.clone()).unwrap_or_default(),
                Self::base_of(state),
            )
        };
        let effective = Arc::new(self.compute(device, &edits, &base)?);
        self.shared()
            .cache
            .insert(device.id.clone(), Arc::clone(&effective));
        Some(effective)
    }

    /// How the device stands.
    ///
    /// Its state is `edited` while it is pending: its configuration differs from its
    /// baseline, or a download stopped after writing had begun, or an old record says it was
    /// programmed with something else. Else `downloaded` when the app knows what it holds,
    /// else `project`.
    fn standing(&self, device: &ProjectDevice) -> view::Standing2<'static> {
        let untouched = view::Standing2 {
            state: "project",
            edits: 0,
            downloaded_ms: None,
            pending_note: None,
        };
        let Some(effective) = self.effective(device) else {
            return untouched;
        };
        let mut shared = self.shared();
        let Some(state) = shared.state.devices.get_mut(&device.id) else {
            return view::Standing2 {
                state: if effective.pending > 0 {
                    "edited"
                } else {
                    "project"
                },
                edits: effective.pending,
                ..untouched
            };
        };
        // A record from before configurations were kept: when the configuration here has
        // its digest, it is the one that was programmed, and the record is completed.
        let mut unknown = false;
        if let Some(record) = state.downloaded.as_mut()
            && record.edits.is_none()
        {
            if effective.digest.as_ref() == Some(&record.digest) {
                record.edits = Some(state.edits.clone());
            } else {
                unknown = true;
            }
        }
        let pending_note = if state.interrupted {
            Some("Programming stopped half-way. Program it again.")
        } else if unknown {
            Some("Differs from what was last programmed.")
        } else {
            None
        };
        let pending = effective.pending > 0 || pending_note.is_some();
        view::Standing2 {
            state: if pending {
                "edited"
            } else if state.downloaded.is_some() {
                "downloaded"
            } else {
                "project"
            },
            edits: effective.pending,
            downloaded_ms: state.downloaded.as_ref().and_then(|record| record.time_ms),
            pending_note,
        }
    }

    fn row(&self, device: &ProjectDevice) -> Value {
        let mut row = view::device_row(device, &self.standing(device));
        let last = self.shared().outcomes.get(&device.id).cloned();
        // What asks for attention, from the last operation on the device: two or three words
        // for the table.
        let attention = last.as_ref().and_then(|job| match (job.kind, job.status) {
            (JobKind::Download, JobStatus::Failed) => Some("Programming failed"),
            (JobKind::Compare, JobStatus::Succeeded) => {
                let result = job.result.as_ref()?;
                if !result["not_loaded"].is_null() {
                    Some("Not loaded")
                } else if result["differing"].as_u64().unwrap_or(0) > 0 {
                    Some("Differs from device")
                } else {
                    None
                }
            }
            _ => None,
        });
        row["attention"] = json!(attention);
        row["last"] = json!(last);
        row
    }

    fn overview(&self) -> Value {
        let project = &self.inner.project;
        let devices: Vec<Value> = project
            .devices
            .iter()
            .map(|device| self.row(device))
            .collect();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "project": { "id": project.id, "name": project.name, "source": self.inner.source },
            "state_file": self.inner.settings.state.as_ref().map(|path| path.display().to_string()),
            "bus": self.inner.bus.describe(),
            "supported": project.devices.iter().filter(|device| device.is_supported()).count(),
            "editable": project.devices.iter().filter(|device| device.is_editable()).count(),
            "devices": devices,
            "job": self.shared().job,
            "queue": self.shared().queue,
        })
    }

    /// What the page builds rooms, things and controls from: every device with its group
    /// objects as they are now and the titles of its parameter pages, the spaces of the
    /// building, and the group addresses with their ranges.
    fn model(&self) -> Value {
        let project = &self.inner.project;
        let devices: Vec<Value> = project
            .devices
            .iter()
            .map(|device| {
                let mut row = self.row(device);
                let effective = self.effective(device);
                let device_view = effective
                    .as_deref()
                    .and_then(|effective| effective.view.as_ref().ok());
                let shared = self.shared();
                let objects = |objects: &[ObjectView]| -> Vec<Value> {
                    objects
                        .iter()
                        .map(|object| view::object_view(project, &shared.state, object))
                        .collect()
                };
                row["objects"] = json!(device_view.map(|view| objects(&view.objects)));
                row["unused"] = json!(device_view.map(|view| objects(&view.unused)));
                row["pages"] = json!(device_view.map(|view| {
                    view.pages
                        .iter()
                        .map(|page| json!({ "id": page.id, "title": page.title, "channel": page.channel }))
                        .collect::<Vec<_>>()
                }));
                row
            })
            .collect();
        let mut model = self.group_addresses();
        model["project"] = json!({ "id": project.id, "name": project.name });
        model["bus"] = self.inner.bus.describe();
        model["spaces"] = json!(
            project
                .spaces
                .iter()
                .map(|space| json!({ "path": space.path, "kind": space.kind }))
                .collect::<Vec<_>>()
        );
        model["devices"] = json!(devices);
        model["job"] = json!(self.shared().job);
        model["queue"] = json!(self.shared().queue);
        model
    }

    /// The group addresses `device` is linked to. For a device this app can edit, those of
    /// the group objects in use; for any other, where what is in use is not for this app to
    /// say, those of every linked object.
    fn linked_addresses(&self, device: &ProjectDevice) -> Vec<GroupAddress> {
        let Some(effective) = self.effective(device) else {
            return Vec::new();
        };
        let Ok(device_view) = &effective.view else {
            return Vec::new();
        };
        let unused = (!device.is_editable())
            .then_some(&device_view.unused)
            .into_iter()
            .flatten();
        device_view
            .objects
            .iter()
            .chain(unused)
            .flat_map(|object| object.links.iter().copied())
            .collect()
    }

    /// The devices `device` talks to: those that share a group address with it.
    fn talks_to(&self, device: &ProjectDevice) -> Vec<String> {
        let own = self.linked_addresses(device);
        if own.is_empty() {
            return Vec::new();
        }
        self.inner
            .project
            .devices
            .iter()
            .filter(|other| other.id != device.id)
            .filter(|other| {
                self.linked_addresses(other)
                    .iter()
                    .any(|address| own.contains(address))
            })
            .map(|other| other.id.clone())
            .collect()
    }

    /// Everything the page needs to show and edit one device.
    fn editor(&self, device: &ProjectDevice) -> Value {
        let project = &self.inner.project;
        let mut payload = self.row(device);
        let Some(effective) = self.effective(device) else {
            return payload;
        };
        let talks_to = self.talks_to(device);
        let shared = self.shared();
        let state = &shared.state;
        match &effective.view {
            Ok(device_view) => {
                let editor = view::editor_view(project, state, device_view);
                payload["pages"] = editor["pages"].clone();
                payload["objects"] = editor["objects"].clone();
                payload["unused"] = editor["unused"].clone();
            }
            Err(error) => payload["view_error"] = json!(error),
        }
        payload["talks_to"] = json!(talks_to);
        payload["changes"] =
            view::changes_view(project, state, &effective.changes, &effective.pages);
        payload["from_project"] =
            view::changes_view(project, state, &effective.from_project, &effective.pages);
        payload["changes"]["dropped"] = json!(
            effective
                .dropped
                .iter()
                .map(|object| view::object_view(project, state, object))
                .collect::<Vec<_>>()
        );
        payload["changes"]["gained"] = json!(
            effective
                .gained
                .iter()
                .map(|object| view::object_view(project, state, object))
                .collect::<Vec<_>>()
        );
        match &effective.download {
            Ok(download) => {
                payload["download"] = json!({
                    "kind": if download.kind.full { "full" } else { "partial" },
                    "falls_back_to_full": download.fallback.is_some(),
                    "phases": download.phases(),
                });
            }
            Err(error) => payload["download_error"] = json!(error),
        }
        if let Some(program) = &device.application {
            payload["capacity"] = json!({
                "addresses": program.address_table.as_ref().and_then(|table| table.max_entries),
                "associations": program
                    .association_table
                    .as_ref()
                    .and_then(|table| table.max_entries),
            });
        }
        if let Some(findings) = shared.findings.get(&device.id) {
            payload["findings"] = json!(view::findings_view(
                project,
                state,
                findings,
                &effective.pages
            ));
        }
        payload
    }

    // ------------------------------------------------------------ editing

    /// Whether an operation is running on the device.
    fn busy(&self, id: &str) -> bool {
        self.shared()
            .job
            .as_ref()
            .is_some_and(|job| job.status == JobStatus::Running && job.device == id)
    }

    /// Write the edits to their file. Returns what went wrong, if anything did.
    fn save(&self, shared: &Shared) -> Option<String> {
        let path = self.inner.settings.state.as_ref()?;
        shared.state.save(path, &self.inner.project.id).err()
    }

    /// Change the device's edits with `change`, check that the result can still be
    /// downloaded, keep it and write it to the file. Returns the editor's new content.
    fn edit(
        &self,
        id: &str,
        change: impl FnOnce(&mut Edits, &ProjectDevice) -> Result<(), EditError>,
    ) -> Result<Value, Refusal> {
        let device = self.device(id)?;
        if !device.is_editable() {
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "this device cannot be programmed here, so it is not edited here either: {}",
                    view::reason(device).unwrap_or_default()
                ),
            ));
        }
        if self.busy(id) {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "an operation is running on this device; wait for it to end",
            ));
        }
        let mut state = self
            .shared()
            .state
            .devices
            .get(id)
            .cloned()
            .unwrap_or_default();
        change(&mut state.edits, device)?;
        let effective = self
            .compute(device, &state.edits, &Self::base_of(Some(&state)))
            .ok_or_else(|| Refusal::new(StatusCode::UNPROCESSABLE_ENTITY, "no application"))?;
        if let Err(error) = &effective.download {
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("that does not fit the device: {error}"),
            ));
        }
        let save_error = {
            let mut shared = self.shared();
            if state.edits.is_empty() && state.downloaded.is_none() && !state.interrupted {
                shared.state.devices.remove(id);
            } else {
                shared.state.devices.insert(id.to_string(), state);
            }
            shared.cache.insert(id.to_string(), Arc::new(effective));
            // What the last comparison found was found against another plan.
            shared.findings.remove(id);
            self.save(&shared)
        };
        let mut payload = self.editor(device);
        payload["saved"] = json!(save_error.is_none());
        payload["save_error"] = json!(save_error);
        self.broadcast(&json!({ "type": "edited", "device": id }));
        Ok(payload)
    }

    /// Parse a list of group addresses given as text.
    fn addresses(links: &Value) -> Result<Vec<GroupAddress>, Refusal> {
        let bad = || Refusal::new(StatusCode::BAD_REQUEST, "links must be a list of addresses");
        links
            .as_array()
            .ok_or_else(bad)?
            .iter()
            .map(|link| {
                let text = link.as_str().ok_or_else(bad)?;
                text.parse().map_err(|_| {
                    Refusal::new(
                        StatusCode::BAD_REQUEST,
                        format!("{text:?} is not a group address"),
                    )
                })
            })
            .collect()
    }

    /// Take over what the last comparison found in the device: every parameter value and
    /// link that can be taken over (`which` = `None`), or those of the named references.
    fn adopt(&self, id: &str, which: Option<Vec<String>>) -> Result<Value, Refusal> {
        let findings = self.shared().findings.get(id).cloned().ok_or_else(|| {
            Refusal::new(
                StatusCode::CONFLICT,
                "compare with the device first; there is nothing to adopt",
            )
        })?;
        let project = &self.inner.project;
        let wanted = |reference: &str| {
            which
                .as_ref()
                .is_none_or(|list| list.iter().any(|r| r == reference))
        };
        self.edit(id, |edits, device| {
            let Some(program) = device.application.as_deref() else {
                return Ok(());
            };
            for finding in &findings {
                match finding {
                    Finding::Parameter {
                        reference,
                        device: value,
                        adoptable: true,
                        ..
                    } if wanted(reference) => {
                        edits.set_parameter(program, &device.data, reference, value)?;
                    }
                    Finding::Links {
                        reference: Some(reference),
                        device: links,
                        ..
                    } if wanted(reference) => edits.set_links(
                        program,
                        &device.data,
                        &project.addresses,
                        reference,
                        links.clone(),
                    )?,
                    _ => {}
                }
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------ group addresses

    /// Every group address the project or the app knows, with who uses it.
    fn group_addresses(&self) -> Value {
        let project = &self.inner.project;
        // Who uses what: from every device's group objects as they are now.
        let mut users: HashMap<GroupAddress, Vec<Value>> = HashMap::new();
        for device in &project.devices {
            let Some(effective) = self.effective(device) else {
                continue;
            };
            let Ok(device_view) = &effective.view else {
                continue;
            };
            for (object, in_use) in device_view
                .objects
                .iter()
                .map(|object| (object, true))
                .chain(device_view.unused.iter().map(|object| (object, false)))
            {
                for (position, address) in object.links.iter().enumerate() {
                    users.entry(*address).or_default().push(json!({
                        "device": device.id,
                        "device_name": device.name,
                        "device_address": device.address.map(|address| address.to_string()),
                        "number": object.number,
                        "object": object.name,
                        "function": object.function,
                        "sending": position == 0,
                        "in_use": in_use,
                    }));
                }
            }
        }
        let shared = self.shared();
        let state = &shared.state;
        let mut all: Vec<(GroupAddress, &'static str)> = project
            .group_addresses
            .iter()
            .map(|entry| {
                let source = if state.renamed.contains_key(&entry.address) {
                    "renamed"
                } else {
                    "project"
                };
                (entry.address, source)
            })
            .collect();
        all.extend(
            state
                .created
                .keys()
                .filter(|address| project.group_address(**address).is_none())
                .map(|address| (*address, "created")),
        );
        // Addresses that are linked but known to neither: adopted from a device.
        let known: Vec<GroupAddress> = all.iter().map(|(address, _)| *address).collect();
        all.extend(
            users
                .keys()
                .filter(|address| !known.contains(address))
                .map(|address| (*address, "unknown")),
        );
        all.sort();
        let addresses: Vec<Value> = all
            .into_iter()
            .map(|(address, source)| {
                let mut entry = view::address_view(project, state, address);
                entry["source"] = json!(source);
                entry["project_name"] = json!(
                    project
                        .group_address(address)
                        .map(|entry| entry.name.clone())
                );
                entry["used_by"] = json!(users.remove(&address).unwrap_or_default());
                entry
            })
            .collect();
        json!({
            "addresses": addresses,
            "ranges": project.group_ranges.iter().map(|range| json!({
                "start": range.start, "end": range.end, "name": range.name, "level": range.level,
            })).collect::<Vec<_>>(),
        })
    }

    fn create_address(&self, body: &Value) -> Result<Value, Refusal> {
        let bad = |message: &str| Refusal::new(StatusCode::UNPROCESSABLE_ENTITY, message);
        let text = body["address"].as_str().unwrap_or_default().trim();
        let address: GroupAddress = text
            .parse()
            .map_err(|_| bad("give the address as main/middle/sub, for example 1/2/3"))?;
        if address.raw() == 0 {
            return Err(bad("0/0/0 is not a group address a device can use"));
        }
        let name = body["name"].as_str().unwrap_or_default().trim();
        if name.is_empty() {
            return Err(bad("give the address a name"));
        }
        let datapoint_type = body["datapoint_type"]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        if let Some(datapoint_type) = datapoint_type {
            let numbers = datapoint_type
                .strip_prefix("DPST-")
                .or_else(|| datapoint_type.strip_prefix("DPT-"));
            let fine = numbers.is_some_and(|numbers| {
                numbers
                    .split('-')
                    .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
            });
            if !fine {
                return Err(bad("a datapoint type looks like DPST-1-1 or DPT-9"));
            }
        }
        let save_error = {
            let mut shared = self.shared();
            if self.inner.project.group_address(address).is_some()
                || shared.state.created.contains_key(&address)
            {
                return Err(Refusal::new(
                    StatusCode::CONFLICT,
                    format!("{address} exists already"),
                ));
            }
            shared.state.created.insert(
                address,
                CreatedAddress {
                    address: address.to_string(),
                    name: name.to_string(),
                    datapoint_type: datapoint_type.map(str::to_string),
                },
            );
            self.save(&shared)
        };
        Ok(json!({
            "address": address.to_string(),
            "saved": save_error.is_none(),
            "save_error": save_error,
        }))
    }

    fn rename_address(&self, address: GroupAddress, body: &Value) -> Result<Value, Refusal> {
        let name = body["name"].as_str().unwrap_or_default().trim().to_string();
        let project_name = self
            .inner
            .project
            .group_address(address)
            .map(|entry| entry.name.clone());
        let save_error = {
            let mut shared = self.shared();
            if let Some(created) = shared.state.created.get_mut(&address) {
                if name.is_empty() {
                    return Err(Refusal::new(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "give the address a name",
                    ));
                }
                created.name = name;
            } else if let Some(project_name) = project_name {
                // The project's own name back, or none given: no rename is left.
                if name.is_empty() || name == project_name {
                    shared.state.renamed.remove(&address);
                } else {
                    shared.state.renamed.insert(address, name);
                }
            } else {
                return Err(Refusal::new(StatusCode::NOT_FOUND, "no such group address"));
            }
            self.save(&shared)
        };
        Ok(json!({ "saved": save_error.is_none(), "save_error": save_error }))
    }

    // ------------------------------------------------------------ operations

    /// Start an operation on the device with identifier `id`. Returns the job's number.
    /// `queued` says that the queue itself asks; while a queue is at work nobody else may.
    fn start(
        &self,
        kind: JobKind,
        id: &str,
        confirmed: bool,
        queued: bool,
    ) -> Result<u64, Refusal> {
        let device = self.device(id)?;
        if !device.is_editable() {
            let reasons: Vec<String> = device.obstacles.iter().map(view::plain_reason).collect();
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "this device cannot be programmed here: {}",
                    reasons.join("; ")
                ),
            ));
        }
        // Editable and comparable, and no more: a comparison only reads.
        if kind == JobKind::Download && !device.is_supported() {
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                view::DOWNLOAD_NOT_ENABLED,
            ));
        }
        let effective = self.effective(device);
        let (Some(address), Some(effective)) = (device.address, effective) else {
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "this device has no plan",
            ));
        };
        let download = match &effective.download {
            Ok(download) => Arc::new(download.clone()),
            Err(error) => {
                return Err(Refusal::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("the download cannot be prepared: {error}"),
                ));
            }
        };
        if matches!(self.inner.bus, Bus::None) {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "no bus is configured: start the server with --gateway, --routing or --simulate",
            ));
        }
        if kind == JobKind::Download && !confirmed {
            return Err(Refusal::new(
                StatusCode::BAD_REQUEST,
                "a download has to be confirmed: send {\"confirm\": true}",
            ));
        }
        let (job, cancel) = {
            let mut shared = self.shared();
            if shared
                .job
                .as_ref()
                .is_some_and(|job| job.status == JobStatus::Running)
                || (!queued && shared.queue.as_ref().is_some_and(|queue| queue.active))
            {
                return Err(Refusal::new(
                    StatusCode::CONFLICT,
                    "another operation is running",
                ));
            }
            let (switch, cancel) = Cancel::new();
            let job = Job {
                id: shared.next_job,
                kind,
                device: device.id.clone(),
                device_name: if device.name.is_empty() {
                    device
                        .product
                        .as_ref()
                        .map(|product| product.text.clone())
                        .unwrap_or_default()
                } else {
                    device.name.clone()
                },
                address: address.to_string(),
                status: JobStatus::Running,
                started_ms: now_ms(),
                finished_ms: None,
                sent: 0,
                repetitions: 0,
                re_requests: 0,
                reconnects: 0,
                recoveries: 0,
                written: None,
                progress: None,
                expected: match kind {
                    JobKind::Download => view::preview(&download, &plan_options())
                        .map_or(0, |requests| requests.len()),
                    // The pre-check as a modelled device answers it, and the reads.
                    JobKind::Compare => {
                        let mut model = SimulatedDevice::holding(&download);
                        let precheck = download
                            .inspect(&mut model, &plan_options())
                            .map_or(0, |found| found.requests.len());
                        precheck + ops::planned_reads(&download).len()
                    }
                },
                phases: match kind {
                    JobKind::Download => download.phases(),
                    JobKind::Compare => {
                        vec![knxide_program::plan::CHECKING, "Reading the device"]
                    }
                },
                phase: None,
                error: None,
                check: None,
                result: None,
            };
            shared.next_job += 1;
            shared.job = Some(job.clone());
            shared.switch = Some(switch);
            (job, cancel)
        };
        self.broadcast(&json!({ "type": "job", "job": job }));
        let id = job.id;
        let programmer = self.clone();
        tokio::spawn(async move {
            programmer
                .run(job, address, download, effective, cancel)
                .await;
        });
        Ok(id)
    }

    async fn run(
        &self,
        job: Job,
        address: IndividualAddress,
        download: Arc<Download>,
        effective: Arc<Effective>,
        cancel: Cancel,
    ) {
        let what = match job.kind {
            JobKind::Compare => "comparing with",
            JobKind::Download => "downloading to",
        };
        self.log(job.id, "info", format!("{what} {address}"));
        let outcome = match &self.inner.bus {
            Bus::None => Err(Failure::plain("no bus is configured".into(), 0)),
            Bus::Simulated(bus, tool) => {
                self.operate(bus.link(*tool), &job, address, download, &effective, cancel)
                    .await
            }
            bus @ (Bus::Access(_) | Bus::AutoRouting(_)) => match bus.access() {
                Some(access) => match ClientLink::open(&access).await {
                    Ok(link) => {
                        self.log(job.id, "info", format!("bus open: {access}"));
                        self.log(
                            job.id,
                            "info",
                            format!("bus open, tool address {}", link.address()),
                        );
                        self.operate(link, &job, address, download, &effective, cancel)
                            .await
                    }
                    Err(message) => Err(Failure::plain(message, 0)),
                },
                None => Err(Failure::plain(link::NO_NETWORK_WARNING.into(), 0)),
            },
        };
        let finished = {
            let mut shared = self.shared();
            shared.switch = None;
            let mut finished = shared.job.clone().unwrap_or(job);
            finished.finished_ms = Some(now_ms());
            finished.phase = None;
            match outcome {
                Ok(result) => {
                    finished.status = if result["complete"] == false {
                        JobStatus::Incomplete
                    } else {
                        // Done: what was sent is all there was to send.
                        finished.expected = finished.sent;
                        JobStatus::Succeeded
                    };
                    finished.result = Some(result);
                }
                Err(failure) => {
                    finished.status = if failure.cancelled {
                        JobStatus::Cancelled
                    } else {
                        JobStatus::Failed
                    };
                    finished.check = failure.check;
                    finished.written = Some(failure.written());
                    finished.progress = failure.progress.as_ref().map(|progress| {
                        json!({
                            "steps": progress.steps,
                            "steps_completed": progress.steps_completed,
                            "failed_step": progress.failed_step,
                            "writes": progress.writes,
                            "last_write": progress.last_write.as_ref().map(ToString::to_string),
                        })
                    });
                    finished.error = Some(failure.message);
                }
            }
            // What is now known about what the device holds.
            let download = finished.kind == JobKind::Download;
            let succeeded = finished.status == JobStatus::Succeeded;
            // A comparison that read everything and found nothing different: the device
            // holds the configuration here, whoever programmed it.
            let same = !download
                && succeeded
                && finished.result.as_ref().is_some_and(|result| {
                    result["differing"] == 0
                        && result["compared"].as_u64().unwrap_or(0) > 0
                        && result["unread_bytes"] == 0
                        && result["refused"] == false
                        && result["not_loaded"].is_null()
                });
            let wrote = download
                && !succeeded
                && finished
                    .progress
                    .as_ref()
                    .is_some_and(|progress| progress["writes"].as_u64().unwrap_or(0) > 0);
            if let Some(digest) = effective
                .digest
                .as_ref()
                .filter(|_| (download && succeeded) || same)
            {
                let state = shared
                    .state
                    .devices
                    .entry(finished.device.clone())
                    .or_default();
                // The configuration that was sent is the baseline from now on: "changed"
                // means different from it.
                let programmed = if download {
                    Some(now_ms())
                } else {
                    state.downloaded.as_ref().and_then(|record| record.time_ms)
                };
                state.downloaded = Some(Downloaded {
                    time_ms: programmed,
                    digest: digest.clone(),
                    edits: Some(state.edits.clone()),
                });
                state.interrupted = false;
                shared.cache.remove(&finished.device);
                if download {
                    shared.findings.remove(&finished.device);
                }
                if let Some(error) = self.save(&shared) {
                    tracing::warn!(%error, "the download could not be noted in the state file");
                }
            } else if wrote {
                // Writing had begun: whatever the values, the device needs programming until
                // a download goes through.
                shared
                    .state
                    .devices
                    .entry(finished.device.clone())
                    .or_default()
                    .interrupted = true;
                if let Some(error) = self.save(&shared) {
                    tracing::warn!(%error, "the stopped download could not be noted in the state file");
                }
            }
            shared.job = Some(finished.clone());
            shared
                .outcomes
                .insert(finished.device.clone(), finished.clone());
            shared.jobs.push(finished.clone());
            if shared.jobs.len() > JOBS_KEPT {
                shared.jobs.remove(0);
            }
            finished
        };
        match (&finished.error, &finished.written) {
            (Some(error), Some(written)) => {
                self.log(finished.id, "error", format!("{error}; {written}"));
            }
            (Some(error), None) => self.log(finished.id, "error", error.clone()),
            (None, _) if finished.status == JobStatus::Incomplete => self.log(
                finished.id,
                "error",
                "gave up; the result covers only part of the memory".into(),
            ),
            (None, _) => self.log(finished.id, "info", "done".into()),
        }
        self.broadcast(&json!({ "type": "job", "job": finished }));
    }

    /// Run the job's operation over `link` and close the connection and the link after it,
    /// whatever happened.
    async fn operate<L: Link>(
        &self,
        link: L,
        job: &Job,
        address: IndividualAddress,
        download: Arc<Download>,
        effective: &Effective,
        mut cancel: Cancel,
    ) -> Result<Value, Failure> {
        let mut session = Session::new(link, address, self.inner.settings.session);
        let options = plan_options();
        let id = job.id;
        let reporter = self.clone();
        let mut report = move |report: Report| match report {
            Report::Request(request) => {
                let sent = {
                    let mut shared = reporter.shared();
                    match shared.job.as_mut() {
                        Some(job) if job.id == id => {
                            job.sent += 1;
                            job.sent
                        }
                        _ => 0,
                    }
                };
                reporter.broadcast(&json!({ "type": "progress", "job": id, "sent": sent }));
                reporter.log(id, "request", request.to_string());
            }
            Report::Response(response) => reporter.log(id, "response", response.to_string()),
            Report::NoResponse => reporter.log(id, "note", "no answer".into()),
            Report::Repeated(count) => reporter.count(id, count, 0, 0),
            Report::AskedAgain(count) => reporter.count(id, 0, count, 0),
            Report::Reconnecting => reporter.count(id, 0, 0, 1),
            Report::Expected(expected) => reporter.expect(id, expected),
            Report::Recovering(failed) => {
                reporter.recovering(id);
                reporter.log(
                    id,
                    "note",
                    format!("{failed}; reading what the device holds instead of stopping"),
                );
            }
            Report::Phase(name) => reporter.phase(id, name),
            Report::Pause(duration) => {
                reporter.log(id, "note", format!("waiting {} ms", duration.as_millis()));
            }
            Report::Note(note) => reporter.log(id, "note", note),
        };
        let project = &self.inner.project;
        let outcome = match job.kind {
            JobKind::Download => {
                let limits = self.inner.settings.download;
                ops::download(
                    &mut session,
                    download,
                    options,
                    limits,
                    &mut report,
                    &mut cancel,
                )
                .await
                .map(|downloaded| {
                    let plan = &downloaded.plan;
                    json!({
                        "kind": if plan.kind.full { "full" } else { "partial" },
                        "full_because": plan.full_because,
                        "requests": plan.requests.len(),
                        "repetitions": downloaded.repetitions,
                        "recoveries": downloaded.recoveries,
                        "serial_number": plan.serial_number.as_deref().map(view::hex),
                        "notes": plan.notes,
                    })
                })
            }
            JobKind::Compare => {
                let limits = self.inner.settings.limits;
                let compared = ops::compare(
                    &mut session,
                    Arc::clone(&download),
                    options,
                    limits,
                    &mut report,
                    &mut cancel,
                )
                .await;
                compared.map(|comparison| {
                    let found = &comparison.found;
                    // What the differing bytes mean: parameters, links, or just bytes.
                    let differences: Vec<compare::Difference> = comparison
                        .differences
                        .iter()
                        .map(|difference| compare::Difference {
                            address: difference.address,
                            device: difference.device.clone(),
                        })
                        .collect();
                    let findings = project
                        .device(&job.device)
                        .and_then(|device| device.application.as_deref())
                        .and_then(|program| {
                            compare::decode(
                                program,
                                &effective.data,
                                download.variant(found.kind),
                                &differences,
                                &comparison.beside,
                            )
                            .ok()
                        })
                        .unwrap_or_default();
                    let mut shared = self.shared();
                    let findings_view =
                        view::findings_view(project, &shared.state, &findings, &effective.pages);
                    shared.findings.insert(job.device.clone(), findings);
                    json!({
                        "kind": if found.kind.full { "full" } else { "partial" },
                        "full_because": found.full_because,
                        "serial_number": found.serial_number.as_deref().map(view::hex),
                        "application": found.device_application.map(|id| id.to_string()),
                        "notes": found.notes,
                        "complete": comparison.is_complete(),
                        "stopped_because": comparison.stopped_because,
                        "refused": comparison.refused,
                        "not_loaded": comparison.not_loaded,
                        "load_states": found.load_states.map(|states| json!({
                            "address_table": states.address_table,
                            "association_table": states.association_table,
                            "application": states.application,
                        })),
                        "planned": comparison.planned,
                        "compared": comparison.compared,
                        "matching": comparison.matching,
                        "differing": comparison.differing,
                        "unread_bytes": comparison.planned - comparison.compared,
                        "unread": comparison.unread.iter().map(|stretch| json!({
                            "address": format!("{:04X}", stretch.address),
                            "length": stretch.length,
                        })).collect::<Vec<_>>(),
                        "repetitions": comparison.repetitions,
                        "re_requests": comparison.re_requests,
                        "reconnects": comparison.reconnects,
                        "requests": comparison.requests,
                        "findings": findings_view,
                        "differences": comparison.differences.iter().map(|difference| json!({
                            "address": format!("{:04X}", difference.address),
                            "planned": view::hex(&difference.planned),
                            "device": view::hex(&difference.device),
                        })).collect::<Vec<_>>(),
                    })
                })
            }
        };
        if session.is_open() {
            self.log(id, "request", "T_DISCONNECT".into());
        }
        session.finish().await.close().await;
        outcome
    }

    /// Add to the running job's counts of repeated frames, answers asked for again and new
    /// connections, and tell the pages.
    fn count(&self, id: u64, repetitions: u32, re_requests: u32, reconnects: u32) {
        let counts = {
            let mut shared = self.shared();
            match shared.job.as_mut() {
                Some(job) if job.id == id => {
                    job.repetitions += repetitions;
                    job.re_requests += re_requests;
                    job.reconnects += reconnects;
                    Some((job.sent, job.repetitions, job.re_requests, job.reconnects))
                }
                _ => None,
            }
        };
        if let Some((sent, repetitions, re_requests, reconnects)) = counts {
            self.broadcast(&json!({
                "type": "progress", "job": id, "sent": sent,
                "repetitions": repetitions, "re_requests": re_requests,
                "reconnects": reconnects,
            }));
        }
    }

    /// The running download recovers from an exchange that failed.
    fn recovering(&self, id: u64) {
        let counts = {
            let mut shared = self.shared();
            match shared.job.as_mut() {
                Some(job) if job.id == id => {
                    job.recoveries += 1;
                    Some((job.sent, job.recoveries))
                }
                _ => None,
            }
        };
        if let Some((sent, recoveries)) = counts {
            self.broadcast(&json!({
                "type": "progress", "job": id, "sent": sent, "recoveries": recoveries,
            }));
        }
    }

    /// The running job now expects to send `expected` requests in all.
    fn expect(&self, id: u64, expected: usize) {
        let sent = {
            let mut shared = self.shared();
            match shared.job.as_mut() {
                Some(job) if job.id == id && job.expected != expected => {
                    job.expected = expected;
                    Some(job.sent)
                }
                _ => None,
            }
        };
        if let Some(sent) = sent {
            self.broadcast(&json!({
                "type": "progress", "job": id, "sent": sent, "expected": expected,
            }));
        }
    }

    /// The running job enters another part.
    fn phase(&self, id: u64, name: &'static str) {
        let sent = {
            let mut shared = self.shared();
            match shared.job.as_mut() {
                Some(job) if job.id == id => {
                    job.phase = Some(name);
                    if !job.phases.contains(&name) {
                        // A partial download that turned into the full one.
                        job.phases.push(name);
                    }
                    Some(job.sent)
                }
                _ => None,
            }
        };
        if let Some(sent) = sent {
            self.broadcast(&json!({ "type": "progress", "job": id, "sent": sent, "phase": name }));
        }
    }

    fn cancel(&self) -> bool {
        // A queue stops with the download that is cancelled.
        self.stop_queue();
        match &self.shared().switch {
            Some(switch) => {
                switch.cancel();
                true
            }
            None => false,
        }
    }
}

/// The HTTP router for a server that serves this one project.
pub fn app(programmer: Programmer) -> Router {
    serve(Workspace::fixed(programmer))
}

/// The HTTP router: the page, the list of projects, and the open project.
pub fn serve(workspace: Workspace) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }),
        )
        .route(
            "/app.css",
            get(|| async { asset("text/css; charset=utf-8", APP_CSS) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route(
            "/model.js",
            get(|| async { asset("text/javascript; charset=utf-8", MODEL_JS) }),
        )
        .route("/fonts/{name}", get(font))
        .route("/api/project", get(project))
        .route("/api/model", get(model))
        .route("/api/devices/{id}", get(device))
        .route(
            "/api/devices/{id}/parameters/{reference}",
            put(set_parameter).delete(reset_parameter),
        )
        .route("/api/devices/{id}/objects/{reference}", put(set_links))
        .route("/api/devices/{id}/edits", axum::routing::delete(discard))
        .route("/api/devices/{id}/adopt", post(adopt))
        .route("/api/devices/{id}/technical", get(technical))
        .route("/api/devices/{id}/plan", get(plan))
        .route("/api/devices/{id}/compare", post(compare))
        .route("/api/devices/{id}/download", post(download))
        .route(
            "/api/group-addresses",
            get(group_addresses).post(create_address),
        )
        .route("/api/group-addresses/{raw}", put(rename_address))
        .route("/api/program", get(queue::status).post(queue::start))
        .route(
            "/api/floor-plans",
            get(floor_plans::list).post(floor_plans::create),
        )
        .route(
            "/api/floor-plans/{id}",
            put(floor_plans::change).delete(floor_plans::remove),
        )
        .route(
            "/api/floor-plans/{id}/image",
            get(floor_plans::image).put(floor_plans::replace_image),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            floor_plans::MAX_IMAGE + 1024,
        ))
        .route("/api/jobs", get(jobs))
        .route("/api/jobs/{id}", get(job_log))
        .route("/api/job", get(job))
        .route("/api/job/cancel", post(cancel))
        .route("/ws", get(ws))
        .merge(workspace::routes())
        .with_state(workspace)
}

/// The typefaces of the page, served from here so that the page asks no one else for them.
const FONTS: [(&str, &[u8]); 6] = [
    (
        "barlow-400.woff2",
        include_bytes!("../static/fonts/barlow-400.woff2"),
    ),
    (
        "barlow-500.woff2",
        include_bytes!("../static/fonts/barlow-500.woff2"),
    ),
    (
        "barlow-600.woff2",
        include_bytes!("../static/fonts/barlow-600.woff2"),
    ),
    (
        "barlow-semi-condensed-500.woff2",
        include_bytes!("../static/fonts/barlow-semi-condensed-500.woff2"),
    ),
    (
        "barlow-semi-condensed-600.woff2",
        include_bytes!("../static/fonts/barlow-semi-condensed-600.woff2"),
    ),
    (
        "jetbrains-mono-400.woff2",
        include_bytes!("../static/fonts/jetbrains-mono-400.woff2"),
    ),
];

async fn font(Path(name): Path<String>) -> Response {
    match FONTS.iter().find(|(known, _)| *known == name) {
        Some((_, bytes)) => (
            [
                (header::CONTENT_TYPE, "font/woff2"),
                (header::CACHE_CONTROL, "public, max-age=604800"),
            ],
            *bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// Whether a request comes from our own page (or from no page at all). Without this check any
/// website open in the same browser could start a download with a plain form post.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) else {
        // Not a browser (command-line tools, tests): no cross-site risk.
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        == Some(host)
}

fn guard(headers: &HeaderMap) -> Result<(), Refusal> {
    if same_origin(headers) {
        Ok(())
    } else {
        Err(Refusal::new(
            StatusCode::FORBIDDEN,
            "requests from other sites are refused",
        ))
    }
}

/// The JSON body of a request; an empty body is an empty object.
fn body_json(body: &str) -> Result<Value, Refusal> {
    if body.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(body)
        .map_err(|_| Refusal::new(StatusCode::BAD_REQUEST, "the body must be JSON"))
}

fn ok(body: &Value) -> Response {
    json_response(StatusCode::OK, body)
}

async fn project(Open(programmer): Open) -> Response {
    ok(&programmer.overview())
}

async fn model(Open(programmer): Open) -> Response {
    ok(&programmer.model())
}

async fn device(Open(programmer): Open, Path(id): Path<String>) -> Result<Response, Refusal> {
    let device = programmer.device(&id)?;
    Ok(ok(&programmer.editor(device)))
}

async fn set_parameter(
    Open(programmer): Open,
    Path((id, reference)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let body = body_json(&body)?;
    let value = match &body["value"] {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => {
            return Err(Refusal::new(
                StatusCode::BAD_REQUEST,
                "send the new value as {\"value\": …}",
            ));
        }
    };
    let payload = programmer.edit(&id, |edits, device| match device.application.as_deref() {
        Some(program) => edits.set_parameter(program, &device.data, &reference, &value),
        None => Ok(()),
    })?;
    Ok(ok(&payload))
}

async fn reset_parameter(
    Open(programmer): Open,
    Path((id, reference)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    // Back to what the device was last programmed with; to the project's value where the
    // app has never programmed it.
    let baseline = programmer.baseline(&id);
    let payload = programmer.edit(&id, |edits, device| {
        let Some(program) = device.application.as_deref() else {
            return Ok(());
        };
        match baseline.parameters.get(program.local_id(&reference)) {
            Some(value) => edits.set_parameter(program, &device.data, &reference, value),
            None => {
                edits.reset_parameter(program, &reference);
                Ok(())
            }
        }
    })?;
    Ok(ok(&payload))
}

async fn set_links(
    Open(programmer): Open,
    Path((id, reference)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let links = Programmer::addresses(&body_json(&body)?["links"])?;
    let project = programmer.project();
    let payload = programmer.edit(&id, |edits, device| match device.application.as_deref() {
        Some(program) => {
            edits.set_links(program, &device.data, &project.addresses, &reference, links)
        }
        None => Ok(()),
    })?;
    Ok(ok(&payload))
}

async fn discard(
    Open(programmer): Open,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    // Back to what the device was last programmed with.
    let baseline = programmer.baseline(&id);
    let payload = programmer.edit(&id, |edits, _| {
        *edits = baseline;
        Ok(())
    })?;
    Ok(ok(&payload))
}

async fn adopt(
    Open(programmer): Open,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let body = body_json(&body)?;
    let which = body["references"].as_array().map(|list| {
        list.iter()
            .filter_map(|reference| reference.as_str().map(str::to_string))
            .collect()
    });
    Ok(ok(&programmer.adopt(&id, which)?))
}

/// The device's effective download, or why there is none.
fn effective_download(
    programmer: &Programmer,
    device: &ProjectDevice,
) -> Result<Arc<Effective>, Refusal> {
    let no_plan = |why: String| {
        Refusal::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("no plan for this device: {why}"),
        )
    };
    let reasons = || {
        device
            .obstacles
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    };
    let effective = programmer
        .effective(device)
        .ok_or_else(|| no_plan(reasons()))?;
    match &effective.download {
        Ok(_) => Ok(effective),
        Err(error) if device.obstacles.is_empty() => Err(no_plan(error.clone())),
        Err(_) => Err(no_plan(reasons())),
    }
}

async fn technical(Open(programmer): Open, Path(id): Path<String>) -> Result<Response, Refusal> {
    let device = programmer.device(&id)?;
    let effective = effective_download(&programmer, device)?;
    let Ok(download) = &effective.download else {
        return Err(Refusal::missing());
    };
    let shared = programmer.shared();
    Ok(ok(&view::technical(
        programmer.project(),
        &shared.state,
        device,
        download,
        &plan_options(),
    )))
}

async fn plan(Open(programmer): Open, Path(id): Path<String>) -> Result<Response, Refusal> {
    let device = programmer.device(&id)?;
    let effective = effective_download(&programmer, device)?;
    let Ok(download) = &effective.download else {
        return Err(Refusal::missing());
    };
    Ok(ok(&json!({
        "id": device.id,
        "address": device.address.map(|address| address.to_string()),
        "supported": device.is_supported(),
        "variants": view::variants(download, &plan_options()),
    })))
}

async fn compare(
    Open(programmer): Open,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let job = programmer.start(JobKind::Compare, &id, false, false)?;
    Ok(json_response(StatusCode::ACCEPTED, &json!({ "job": job })))
}

async fn download(
    Open(programmer): Open,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let confirmed = body_json(&body)?["confirm"] == true;
    let job = programmer.start(JobKind::Download, &id, confirmed, false)?;
    Ok(json_response(StatusCode::ACCEPTED, &json!({ "job": job })))
}

async fn group_addresses(Open(programmer): Open) -> Response {
    ok(&programmer.group_addresses())
}

async fn create_address(
    Open(programmer): Open,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let created = programmer.create_address(&body_json(&body)?)?;
    Ok(json_response(StatusCode::CREATED, &created))
}

async fn rename_address(
    Open(programmer): Open,
    Path(raw): Path<u16>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let renamed = programmer.rename_address(GroupAddress::from_raw(raw), &body_json(&body)?)?;
    Ok(ok(&renamed))
}

async fn jobs(Open(programmer): Open) -> Response {
    let shared = programmer.shared();
    let mut all = shared.jobs.clone();
    if let Some(running) = shared
        .job
        .as_ref()
        .filter(|job| job.status == JobStatus::Running)
    {
        all.push(running.clone());
    }
    all.reverse();
    ok(&json!({ "jobs": all }))
}

async fn job_log(Open(programmer): Open, Path(id): Path<u64>) -> Result<Response, Refusal> {
    let shared = programmer.shared();
    let job = shared
        .jobs
        .iter()
        .chain(shared.job.iter())
        .find(|job| job.id == id)
        .ok_or_else(|| Refusal::new(StatusCode::NOT_FOUND, "no such job"))?;
    let log: Vec<&LogLine> = shared.log.iter().filter(|line| line.job == id).collect();
    Ok(ok(&json!({ "job": job, "log": log })))
}

async fn job(Open(programmer): Open) -> Response {
    let shared = programmer.shared();
    let id = shared.job.as_ref().map(|job| job.id);
    let log: Vec<&LogLine> = shared
        .log
        .iter()
        .filter(|line| Some(line.job) == id)
        .collect();
    ok(&json!({ "job": shared.job, "log": log }))
}

async fn cancel(Open(programmer): Open, headers: HeaderMap) -> Result<Response, Refusal> {
    guard(&headers)?;
    Ok(ok(&json!({ "cancelled": programmer.cancel() })))
}

async fn ws(upgrade: WebSocketUpgrade, headers: HeaderMap, Open(programmer): Open) -> Response {
    if !same_origin(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin WebSocket refused").into_response();
    }
    upgrade.on_upgrade(move |socket| session(socket, programmer))
}

async fn session(mut socket: WebSocket, programmer: Programmer) {
    // Subscribe before the snapshot so nothing falls between them.
    let mut events = programmer.inner.events.subscribe();
    let hello = {
        let shared = programmer.shared();
        let id = shared.job.as_ref().map(|job| job.id);
        let log: Vec<&LogLine> = shared
            .log
            .iter()
            .filter(|line| Some(line.job) == id)
            .collect();
        json!({ "type": "hello", "job": shared.job, "queue": shared.queue, "log": log }).to_string()
    };
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            event = events.recv() => match event {
                Ok(text) => {
                    if socket.send(Message::Text(text.as_str().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "browser too slow; events skipped");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}
