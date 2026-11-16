//! Turning a prepared [`Download`] into the ordered list of bus requests.
//!
//! Some requests depend on what the device answered before (the load state after an event, the
//! device control byte, memory content that is read before it is written). The planner
//! therefore runs against a [`Device`]: something that answers requests. For a replay that is a
//! recording ([`crate::SimulatedDevice`]); a bus driver can implement the trait as well.
//! Nothing in this crate opens a connection.
//!
//! # Sequence
//!
//! 1. Opening (verified, identical in three recorded downloads): connect, read the device
//!    descriptor, read the maximum APDU length (object 0, property 56), authorise with the
//!    access key.
//! 2. Pre-check, generated from what the mask offers ([`crate::MaskProfile`]); every read of
//!    the three recorded downloads comes out of these rules, in this order:
//!    1. the serial number is read; a failure is tolerated and nothing is compared;
//!    2. before a partial download the device is asked whether it holds the application: its
//!       load state, its identity, the state of a second application. If not, a partial
//!       download made by [`Download::prepare`] turns into the full one and an explicit one
//!       stops;
//!    3. the manufacturer is read and must be the application's, else the run stops;
//!    4. the hardware identification is read and must fit the image, else the run stops;
//!    5. before a partial download the checksums of the last download are compared, when the
//!       project holds them.
//!
//!    Nothing has been written when a check stops the run. The rules are from behaviour
//!    notes; the recordings show them passing, never failing.
//! 3. The steps of the procedure, in order. A step that needs the device after a restart
//!    reconnects: a pause, connect, device descriptor; the APDU length is not read again and
//!    the key is presented again only when it is not the default one (from behaviour notes,
//!    never seen on a bus).
//!
//! [`Download::precheck`] stops after the second part, with the connection open.

use std::fmt;
use std::time::Duration;

use crate::download::{ApplicationId, Download};
use crate::mask::{LoadControl, Resource};
use crate::procedure::{Action, ControlVariable, DownloadKind, ErrorCause, LoadStep, Target};
use crate::request::{Request, Response};

/// Property of every load state machine object that takes load events (load state control).
const PID_LOAD_STATE_CONTROL: u8 = 5;
/// Property of the device object whose bit 2 switches on echoing of memory writes.
const PID_DEVICE_CONTROL: u8 = 14;
/// Property that lists an object's memory control blocks, eight bytes each.
const PID_MCB_TABLE: u8 = 27;
/// Property of the device object with the longest APDU the device accepts.
const PID_MAX_APDU_LENGTH: u8 = 56;
/// The verify-mode bit of the device control property.
const VERIFY_MODE: u8 = 0x04;
/// APDU length every device supports.
const STANDARD_APDU_LENGTH: u8 = 15;
/// The access key of a device nobody has locked.
const DEFAULT_KEY: u32 = 0xFFFF_FFFF;
/// How long a restarted device is given before the next connect. Behaviour notes describe
/// attempts from one second on until three seconds have passed; this crate makes one attempt
/// at the end of that window.
const RESTART_PAUSE: Duration = Duration::from_secs(3);
/// Application programs whose devices are not asked for their serial number, by the start of
/// the program's identifier. From behaviour notes; unverified.
const NO_SERIAL_NUMBER: [&str; 8] = [
    "M-004E_A-0002-11",
    "M-004E_A-0003-10",
    "M-004E_A-0003-20",
    "M-004E_A-0020-10",
    "M-004E_A-0020-20",
    "M-004E_A-0030-10",
    "M-004E_A-0101-13",
    "M-004E_A-1001-20",
];

/// Load state machine states. Spec: 3/5/2 (Management Procedures), load state machine.
mod state {
    pub(super) const UNLOADED: u8 = 0;
    pub(super) const LOADED: u8 = 1;
    pub(super) const LOADING: u8 = 2;
    pub(super) const UNLOADING: u8 = 4;
    pub(super) const LOAD_COMPLETING: u8 = 5;
}

/// Load event codes: byte 0 of a load state control write.
mod event {
    pub(super) const LOAD: u8 = 1;
    pub(super) const LOAD_COMPLETED: u8 = 2;
    pub(super) const SEGMENT: u8 = 3;
    pub(super) const UNLOAD: u8 = 4;
}

/// Whether the planner waits for an answer to a request it hands to a [`Device`].
///
/// Most requests say so themselves; a memory write does not, because a device echoes it only
/// while its verify mode is on. A bus driver needs to know whether to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// The device should answer.
    Answer,
    /// No answer is expected: the request is complete once the device has taken it.
    Nothing,
}

/// The device can no longer be talked to: the connection broke, or the run was cancelled.
/// Planning stops at once with [`PlanError::Bus`]; nothing more is handed to the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault(pub String);

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fault {}

/// Something that answers requests: a recording, a model or a real device behind a driver.
pub trait Device {
    /// Handle one request and return the device's answer, or `None` when it gives none (a
    /// transport request, a restart, an unverified memory write, a timeout).
    ///
    /// `expect` tells a driver whether to wait for an answer; a recording or a model can
    /// ignore it. An `Err` means the device is out of reach and ends the run.
    fn exchange(&mut self, request: &Request, expect: Expect) -> Result<Option<Response>, Fault>;

    /// Let `duration` pass before the next request (a delay step of the procedure). A recording
    /// or a model has nothing to wait for, so the default returns at once.
    fn pause(&mut self, duration: Duration) -> Result<(), Fault> {
        let _ = duration;
        Ok(())
    }

    /// The run enters another part: [`CHECKING`] first, then the parts the procedure's steps
    /// belong to ([`crate::procedure::Action::phase`]). For showing progress; the default
    /// ignores it.
    fn phase(&mut self, name: &'static str) {
        let _ = name;
    }
}

/// The part of a run before the procedure: the opening and the pre-check.
pub const CHECKING: &str = "Checking the device";

/// Settings of one planning run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanOptions {
    /// Access key presented after connecting.
    pub key: u32,
    /// Longest APDU the tool and its interface handle. Only 15 is verified.
    pub max_apdu_length: u8,
    /// Stop, at the end of the pre-check, when the device reports another application program
    /// than the one being downloaded. A device that does not tell passes.
    ///
    /// Off by default, because the recorded tool does not do this: it answers another
    /// application with a full download. Turning it on adds one read of the application's
    /// identity where the pre-check has not already made it.
    pub require_same_application: bool,
    /// How often a load state is read again while the device reports a transitional state.
    pub max_state_polls: u16,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            key: DEFAULT_KEY,
            max_apdu_length: STANDARD_APDU_LENGTH,
            require_same_application: false,
            // Thirty seconds at one read every 100 ms.
            max_state_polls: 300,
        }
    }
}

/// The outcome of planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Every request, in order.
    pub requests: Vec<Request>,
    /// APDU length the plan was made for.
    pub apdu_length: u8,
    /// The kind of download that was planned: for a download made by [`Download::prepare`],
    /// what the device's answers decided. [`Download::variant`] gives the matching image.
    pub kind: DownloadKind,
    /// Why a partial download was not possible and the full one was planned instead.
    pub full_because: Option<String>,
    /// The serial number the device reported, if it did.
    pub serial_number: Option<Vec<u8>>,
    /// The application the device reported: manufacturer, application number, version.
    /// `None` when the device was not asked or does not tell.
    pub device_application: Option<ApplicationId>,
    /// Result of the checksum comparison before a partial download: `Some(true)` when the
    /// device still holds what the project recorded, `None` when nothing was compared. A match
    /// would let a tool skip unchanged bytes; this crate plans the plain partial download
    /// either way.
    pub stamp_matches: Option<bool>,
    /// Things worth telling the user that did not stop the run.
    pub notes: Vec<String>,
    /// The states of the device's load state machines, where they were asked for: only
    /// [`Download::inspect`] does.
    pub load_states: Option<LoadStates>,
}

/// The states of the three load state machines an application download goes through, as a
/// device reports them: 0 unloaded, 1 loaded, 2 loading, 3 error, 4 unloading, 5 load
/// completing. `None` where the device gave no state (some devices are managed without load
/// state machines).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoadStates {
    /// The group address table (object index 1).
    pub address_table: Option<u8>,
    /// The association table (object index 2).
    pub association_table: Option<u8>,
    /// The application program (object index 3).
    pub application: Option<u8>,
}

impl LoadStates {
    fn parts(&self) -> [(&'static str, Option<u8>); 3] {
        [
            ("address table", self.address_table),
            ("association table", self.association_table),
            ("application", self.application),
        ]
    }

    /// Whether nothing speaks against the device being loaded: every state it gave is
    /// "loaded". A device that gave none counts as loaded; there is nothing to go by.
    pub fn is_loaded(&self) -> bool {
        self.parts()
            .iter()
            .all(|(_, state)| state.is_none_or(|state| state == state::LOADED))
    }

    /// For a device that is not loaded, what it lacks, in a sentence; `None` for a loaded
    /// one. Such a device answers reads of its memory with no data, or holds leftovers: there
    /// is nothing to compare, and it needs a full download.
    pub fn not_loaded(&self) -> Option<String> {
        if self.is_loaded() {
            return None;
        }
        let word = |state: u8| match state {
            state::UNLOADED => "unloaded".to_string(),
            state::LOADED => "loaded".to_string(),
            state::LOADING => "being loaded".to_string(),
            state::UNLOADING => "being unloaded".to_string(),
            state::LOAD_COMPLETING => "completing its load".to_string(),
            3 => "in error".to_string(),
            other => format!("in state {other}"),
        };
        let known: Vec<(&str, u8)> = self
            .parts()
            .iter()
            .filter_map(|(name, state)| Some((*name, (*state)?)))
            .collect();
        if known.len() == 3 && known.iter().all(|(_, state)| *state == state::UNLOADED) {
            return Some(
                "the device holds no application: its address table, association table and \
                 application are unloaded; it needs a full download"
                    .into(),
            );
        }
        let states: Vec<String> = known
            .iter()
            .map(|(name, state)| format!("its {name} is {}", word(*state)))
            .collect();
        Some(format!(
            "the device is partly loaded: {}; it needs a full download",
            states.join(", ")
        ))
    }
}

/// Why planning stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The device gave no answer where one is required.
    NoResponse(Request),
    /// The answer does not belong to the request.
    UnexpectedResponse {
        /// The request.
        request: Request,
        /// What came back.
        response: Response,
    },
    /// The device reports another mask version than the application is written for.
    WrongMask {
        /// Mask version of the application.
        expected: u16,
        /// Mask version of the device.
        actual: u16,
    },
    /// The device reports another application program than the one being downloaded, and the
    /// caller required the same ([`PlanOptions::require_same_application`]).
    WrongApplication {
        /// Identity of the application being downloaded.
        expected: ApplicationId,
        /// What the device reports.
        actual: ApplicationId,
    },
    /// The device is of another manufacturer than the application. Nothing was written.
    WrongManufacturer {
        /// Manufacturer of the application.
        expected: u16,
        /// Manufacturer the device reports.
        actual: u16,
    },
    /// The device's hardware identification does not fit the image. Nothing was written.
    WrongHardware {
        /// What the device reports.
        device: Vec<u8>,
        /// What the image holds.
        image: Vec<u8>,
    },
    /// A partial download was asked for explicitly and the device does not hold the
    /// application. The string says what was found. Nothing was written.
    PartialNotPossible(String),
    /// The access level the key grants is not enough (a level above 2).
    AccessDenied {
        /// The granted level.
        level: u8,
    },
    /// A load state machine did not reach the state an event should lead to.
    LoadState {
        /// Object index of the state machine.
        object: u8,
        /// The state the event should lead to.
        expected: u8,
        /// The state the device reports.
        actual: u8,
    },
    /// Written data read back differently, or a comparison step found other data.
    Mismatch(Request),
    /// The device has no such property, or refuses access to it.
    ResourceNotFound(Request),
    /// The procedure contains something the planner has no rules for.
    Unsupported(String),
    /// The device went out of reach; see [`Fault`].
    Bus(String),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoResponse(request) => write!(f, "no answer to {request}"),
            Self::UnexpectedResponse { request, response } => {
                write!(f, "{response} does not answer {request}")
            }
            Self::WrongMask { expected, actual } => write!(
                f,
                "the device has mask {actual:04X}, the application needs {expected:04X}"
            ),
            Self::WrongApplication { expected, actual } => write!(
                f,
                "application check failed: the device holds {actual}, the download is for \
                 {expected}"
            ),
            Self::WrongManufacturer { expected, actual } => write!(
                f,
                "manufacturer check failed: the device reports {actual:04X}, the application \
                 is for {expected:04X}"
            ),
            Self::WrongHardware { device, image } => {
                write!(f, "hardware check failed: the device reports ")?;
                device.iter().try_for_each(|byte| write!(f, "{byte:02X}"))?;
                write!(f, ", the application is for ")?;
                image.iter().try_for_each(|byte| write!(f, "{byte:02X}"))
            }
            Self::PartialNotPossible(what) => {
                write!(f, "a partial download is not possible: {what}")
            }
            Self::AccessDenied { level } => write!(f, "access level {level} is not sufficient"),
            Self::LoadState {
                object,
                expected,
                actual,
            } => write!(
                f,
                "load state machine {object} is in state {actual}, expected {expected}"
            ),
            Self::Mismatch(request) => write!(f, "unexpected data after {request}"),
            Self::ResourceNotFound(request) => write!(f, "{request}: no such resource"),
            Self::Unsupported(what) => write!(f, "not supported: {what}"),
            Self::Bus(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for PlanError {}

impl From<Fault> for PlanError {
    fn from(fault: Fault) -> Self {
        Self::Bus(fault.0)
    }
}

impl PlanError {
    fn cause(&self) -> Option<ErrorCause> {
        match self {
            Self::Mismatch(_) => Some(ErrorCause::CompareMismatch),
            Self::ResourceNotFound(_) => Some(ErrorCause::ResourceNotFound),
            _ => None,
        }
    }
}

/// How far a run got: what a device has and has not been told when a download stops.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Progress {
    /// Number of steps of the procedure that ran.
    pub steps: usize,
    /// How many of them were completed.
    pub steps_completed: usize,
    /// The step that was under way when the run stopped; `None` when it stopped before the
    /// procedure (in the opening or the pre-check) or after its last step.
    pub failed_step: Option<String>,
    /// Number of writes (memory and property) the device has taken.
    pub writes: usize,
    /// The last write the device has taken: acknowledged, and answered where it has an answer.
    pub last_write: Option<Request>,
}

impl fmt::Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(last) = &self.last_write else {
            return f.write_str("nothing was written to the device");
        };
        match &self.failed_step {
            Some(step) => write!(
                f,
                "stopped in step {} of {} ({step}); {} steps were completed",
                self.steps_completed + 1,
                self.steps,
                self.steps_completed
            )?,
            None => write!(
                f,
                "{} of {} steps were completed",
                self.steps_completed, self.steps
            )?,
        }
        write!(
            f,
            "; the device took {} write{}, the last one {last}",
            self.writes,
            if self.writes == 1 { "" } else { "s" }
        )
    }
}

/// Planning stopped early. The requests up to that point are kept for diagnosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFailure {
    /// What went wrong.
    pub error: PlanError,
    /// The requests issued before the failure, the failing one included.
    pub requests: Vec<Request>,
    /// How far the run got.
    pub progress: Box<Progress>,
}

impl fmt::Display for PlanFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "planning stopped after {} requests: {}",
            self.requests.len(),
            self.error
        )
    }
}

impl std::error::Error for PlanFailure {}

impl Download {
    /// The parts a run of this download goes through, in order: [`CHECKING`], then the parts
    /// of the procedure. A partial download that turns into its fallback goes through the
    /// fallback's parts instead.
    pub fn phases(&self) -> Vec<&'static str> {
        let mut phases = vec![CHECKING];
        for step in &self.procedure {
            if let Some(name) = step.action.phase()
                && phases.last() != Some(&name)
            {
                phases.push(name);
            }
        }
        phases
    }

    /// Run the opening, the pre-check and the procedure against `device` and collect the
    /// requests.
    pub fn plan(
        &self,
        device: &mut dyn Device,
        options: &PlanOptions,
    ) -> Result<Plan, PlanFailure> {
        Executor::new(self, device, options).finish(|executor| executor.run())
    }

    /// Run the opening and the pre-check only: everything a download does before its first
    /// procedure step, all of it reading. The connection is left open.
    ///
    /// The result says which kind of download the device's answers lead to; a failure names
    /// the check that stopped it.
    pub fn precheck(
        &self,
        device: &mut dyn Device,
        options: &PlanOptions,
    ) -> Result<Plan, PlanFailure> {
        Executor::new(self, device, options).finish(|executor| executor.ensure_connected())
    }

    /// Like [`Download::precheck`], and after it the states of the load state machines for
    /// the address table, the association table and the application are read: the plan's
    /// [`Plan::load_states`]. All of it reading; the connection is left open.
    ///
    /// For a tool that wants to look at a device without downloading: one that is unloaded,
    /// or was left half-way by a download that stopped, says so here, where reading its
    /// memory would only be refused. The reads are those a download makes to follow its load
    /// events (verified against recorded downloads); making them without a load event before
    /// is this crate's own use of them.
    pub fn inspect(
        &self,
        device: &mut dyn Device,
        options: &PlanOptions,
    ) -> Result<Plan, PlanFailure> {
        Executor::new(self, device, options).finish(|executor| {
            executor.ensure_connected()?;
            executor.read_load_states()
        })
    }
}

/// How a verified memory write checks its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    /// Write only.
    Plain,
    /// The device echoes each write.
    Echo,
    /// Write, then read back.
    ReadBack,
    /// Read first, write what differs, read that back.
    Optimistic,
}

struct Executor<'a> {
    /// The download being planned; a partial one is swapped for its fallback when the device
    /// does not hold the application.
    download: &'a Download,
    device: &'a mut dyn Device,
    options: &'a PlanOptions,
    requests: Vec<Request>,
    connected: bool,
    /// Whether the first connect, with the pre-check, has happened.
    opened: bool,
    /// Whether the connection was last closed by a restart.
    restarted: bool,
    apdu_length: u8,
    /// Last known value of the device control property on this connection.
    device_control: Option<u8>,
    /// The device control property could not be read; echo mode is off the table.
    echo_unavailable: bool,
    segment_write: bool,
    optimistic_write: bool,
    memory_auto_verify: Option<bool>,
    full_because: Option<String>,
    serial_number: Option<Vec<u8>>,
    device_application: Option<ApplicationId>,
    stamp_matches: Option<bool>,
    notes: Vec<String>,
    load_states: Option<LoadStates>,
    /// Whether the steps of the procedure have begun.
    in_procedure: bool,
    steps_completed: usize,
    writes: usize,
    last_write: Option<Request>,
}

impl<'a> Executor<'a> {
    fn new(download: &'a Download, device: &'a mut dyn Device, options: &'a PlanOptions) -> Self {
        Self {
            download,
            device,
            options,
            requests: Vec::new(),
            connected: false,
            opened: false,
            restarted: false,
            apdu_length: STANDARD_APDU_LENGTH,
            device_control: None,
            echo_unavailable: false,
            segment_write: true,
            optimistic_write: true,
            memory_auto_verify: None,
            full_because: None,
            serial_number: None,
            device_application: None,
            stamp_matches: None,
            notes: Vec::new(),
            load_states: None,
            in_procedure: false,
            steps_completed: 0,
            writes: 0,
            last_write: None,
        }
    }

    fn finish(
        mut self,
        work: impl FnOnce(&mut Self) -> Result<(), PlanError>,
    ) -> Result<Plan, PlanFailure> {
        match work(&mut self) {
            Ok(()) => Ok(Plan {
                requests: self.requests,
                apdu_length: self.apdu_length,
                kind: self.download.kind,
                full_because: self.full_because,
                serial_number: self.serial_number,
                device_application: self.device_application,
                stamp_matches: self.stamp_matches,
                notes: self.notes,
                load_states: self.load_states,
            }),
            Err(error) => Err(PlanFailure {
                error,
                requests: self.requests,
                progress: Box::new(Progress {
                    steps: self.download.procedure.len(),
                    steps_completed: self.steps_completed,
                    failed_step: self
                        .download
                        .procedure
                        .get(self.steps_completed)
                        .filter(|_| self.in_procedure)
                        .map(|step| step.action.to_string()),
                    writes: self.writes,
                    last_write: self.last_write,
                }),
            }),
        }
    }

    /// Hand one request to the device. A memory write is answered only by a device known to
    /// be in verify mode; every other request says itself whether it has an answer.
    fn send(&mut self, request: Request) -> Result<Option<Response>, PlanError> {
        let echoing = self
            .device_control
            .is_some_and(|control| control & VERIFY_MODE != 0);
        let expect = match &request {
            Request::Connect | Request::Disconnect | Request::Restart => Expect::Nothing,
            Request::MemoryWrite { .. } if !echoing => Expect::Nothing,
            _ => Expect::Answer,
        };
        let response = self.device.exchange(&request, expect);
        self.requests.push(request);
        let response = response?;
        if let Some(write @ (Request::MemoryWrite { .. } | Request::PropertyWrite { .. })) =
            self.requests.last()
        {
            self.writes += 1;
            self.last_write = Some(write.clone());
        }
        Ok(response)
    }

    fn last_request(&self) -> Request {
        self.requests.last().cloned().unwrap_or(Request::Connect)
    }

    fn run(&mut self) -> Result<(), PlanError> {
        // The pre-check may swap the download for its fallback, so it has to be over before
        // the first step of either procedure is looked at.
        if self
            .download
            .procedure
            .iter()
            .any(|step| step.action.needs_device())
        {
            self.ensure_connected()?;
        }
        let download = self.download;
        self.in_procedure = true;
        let mut phase = None;
        for (at, step) in download.procedure.iter().enumerate() {
            if let Some(name) = step.action.phase()
                && phase != Some(name)
            {
                phase = Some(name);
                self.device.phase(name);
            }
            match self.execute(step) {
                Ok(()) => {}
                Err(error)
                    if error
                        .cause()
                        .is_some_and(|cause| step.ignore.contains(&cause)) => {}
                Err(error) => return Err(error),
            }
            self.steps_completed = at + 1;
        }
        Ok(())
    }

    fn execute(&mut self, step: &LoadStep) -> Result<(), PlanError> {
        match &step.action {
            Action::Connect => self.ensure_connected(),
            Action::Disconnect => {
                if self.connected {
                    self.connected = false;
                    self.send(Request::Disconnect)?;
                }
                Ok(())
            }
            Action::Restart => {
                self.ensure_connected()?;
                self.send(Request::Restart)?;
                self.connected = false;
                self.restarted = true;
                self.device_control = None;
                self.send(Request::Disconnect)?;
                Ok(())
            }
            Action::Delay { milliseconds } => {
                // Nothing is sent, but a device behind a bus needs the time.
                self.device
                    .pause(Duration::from_millis(u64::from(*milliseconds)))?;
                Ok(())
            }
            Action::SetControlVariable { variable, value } => {
                match variable {
                    ControlVariable::SegmentWrite => self.segment_write = *value,
                    ControlVariable::OptimisticWrite => self.optimistic_write = *value,
                    ControlVariable::MemoryAutoVerify => self.memory_auto_verify = Some(*value),
                    ControlVariable::VerifyOnWriteDirect => {}
                }
                Ok(())
            }
            Action::Unload { target } => {
                self.load_event(*target, event::UNLOAD, [0; 9], state::UNLOADED)
            }
            Action::Load { target } => {
                self.load_event(*target, event::LOAD, [0; 9], state::LOADING)
            }
            Action::LoadCompleted { target } => {
                self.load_event(*target, event::LOAD_COMPLETED, [0; 9], state::LOADED)
            }
            Action::AbsSegment {
                target,
                segment_type,
                address,
                size,
                access,
                memory_type,
                flags,
            } => {
                let [a1, a2] = address.to_be_bytes();
                let [s1, s2] = size.to_be_bytes();
                let info = [
                    *segment_type,
                    a1,
                    a2,
                    s1,
                    s2,
                    *access,
                    *memory_type,
                    *flags,
                    0,
                ];
                self.load_event(*target, event::SEGMENT, info, state::LOADING)?;
                // An EEPROM segment the image knows about is written right away.
                let in_image = self
                    .download
                    .image
                    .covers_any(u32::from(*address), u32::from(*size));
                if self.segment_write && memory_type & 3 == 3 && in_image {
                    self.write_memory(*address, *size, true, None)?;
                }
                Ok(())
            }
            Action::TaskSegment { target, address } => {
                let [a1, a2] = address.to_be_bytes();
                let [m1, m2, n1, n2, version] = self.download.application.to_bytes();
                let info = [
                    0x02,
                    a1,
                    a2,
                    self.download.pei_type,
                    m1,
                    m2,
                    n1,
                    n2,
                    version,
                ];
                self.load_event(*target, event::SEGMENT, info, state::LOADING)
            }
            Action::TaskPtr {
                target,
                init,
                save,
                serial,
            } => {
                let [i1, i2] = init.to_be_bytes();
                let [s1, s2] = save.to_be_bytes();
                let [r1, r2] = serial.to_be_bytes();
                let info = [0x03, i1, i2, s1, s2, r1, r2, 0, 0];
                self.load_event(*target, event::SEGMENT, info, state::LOADING)
            }
            Action::TaskCtrl1 {
                target,
                address,
                count,
            } => {
                let [a1, a2] = address.to_be_bytes();
                let info = [0x04, a1, a2, *count, 0, 0, 0, 0, 0];
                self.load_event(*target, event::SEGMENT, info, state::LOADING)
            }
            Action::TaskCtrl2 {
                target,
                callback,
                address,
                segment0,
                segment1,
            } => {
                let [c1, c2] = callback.to_be_bytes();
                let [a1, a2] = address.to_be_bytes();
                let [p1, p2] = segment0.to_be_bytes();
                let [q1, q2] = segment1.to_be_bytes();
                let info = [0x05, c1, c2, a1, a2, p1, p2, q1, q2];
                self.load_event(*target, event::SEGMENT, info, state::LOADING)
            }
            Action::WriteProperty {
                object,
                property,
                start,
                count,
                verify,
                inline,
            } => self.write_property(*object, *property, *start, *count, *verify, inline),
            Action::CompareProperty {
                object,
                property,
                start,
                count,
                inline,
                mask,
                invert,
            } => self.compare_property(*object, *property, *start, *count, inline, mask, *invert),
            Action::LoadImageProperty {
                object,
                property,
                start,
                count,
            } => {
                let (object, property, _) = self.property_target(*object, *property)?;
                self.read_property(object, property, *start, *count)
                    .map(|_| ())
            }
            Action::WriteMemory {
                address,
                size,
                verify,
                inline,
            } => self.write_memory(*address, *size, *verify, inline.as_deref()),
            Action::Unsupported { element } => {
                Err(PlanError::Unsupported(format!("step {element}")))
            }
        }
    }

    // ---------------------------------------------------------------- connection

    fn ensure_connected(&mut self) -> Result<(), PlanError> {
        if self.connected {
            return Ok(());
        }
        if std::mem::take(&mut self.restarted) {
            self.device.pause(RESTART_PAUSE)?;
        }
        if !self.opened {
            self.device.phase(CHECKING);
        }
        self.send(Request::Connect)?;
        self.connected = true;
        self.device_control = None;
        let expected = self.download.profile.mask_version;
        match self.send(Request::DeviceDescriptorRead)? {
            Some(Response::DeviceDescriptor { mask_version }) if mask_version == expected => {}
            Some(Response::DeviceDescriptor { mask_version }) => {
                return Err(PlanError::WrongMask {
                    expected,
                    actual: mask_version,
                });
            }
            other => return Err(self.wrong_answer(other)),
        }
        if self.opened {
            // A reconnect: the APDU length is known, and only a key of its own is presented
            // again. From behaviour notes; never seen in a recording.
            if self.options.key != DEFAULT_KEY {
                self.authorize()?;
            }
            return Ok(());
        }
        self.opened = true;

        // Maximum APDU length: absent (count 0) or implausible values mean the standard 15.
        let reported = match self.send(Request::PropertyRead {
            object: 0,
            property: PID_MAX_APDU_LENGTH,
            start: 1,
            count: 1,
        })? {
            Some(Response::Property { count, data, .. }) if count > 0 => match data.as_slice() {
                [high, low] => Some(u16::from_be_bytes([*high, *low])),
                _ => None,
            },
            _ => None,
        };
        let reported = reported
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value != 0 && *value != u8::MAX)
            .unwrap_or(STANDARD_APDU_LENGTH);
        self.apdu_length = reported
            .min(self.options.max_apdu_length)
            .max(STANDARD_APDU_LENGTH);
        self.authorize()?;

        self.read_serial_number()?;
        if !self.download.kind.full {
            self.check_partial_possible()?;
        }
        self.check_manufacturer()?;
        self.check_hardware()?;
        self.compare_stamp()?;
        if self.options.require_same_application {
            self.check_same_application()?;
        }
        Ok(())
    }

    fn authorize(&mut self) -> Result<(), PlanError> {
        match self.send(Request::Authorize {
            key: self.options.key,
        })? {
            Some(Response::Authorize { level }) if level <= 2 => Ok(()),
            Some(Response::Authorize { level }) => Err(PlanError::AccessDenied { level }),
            other => Err(self.wrong_answer(other)),
        }
    }

    // ---------------------------------------------------------------- pre-check

    /// Read `length` bytes of a mask resource. `Ok(None)` when the device says the resource is
    /// not there (a property answered with no elements); a missing or foreign answer is an
    /// error.
    fn read_resource(
        &mut self,
        resource: Resource,
        length: usize,
    ) -> Result<Option<Vec<u8>>, PlanError> {
        match resource {
            Resource::Property { object, property } => {
                let answer = self.send(Request::PropertyRead {
                    object,
                    property,
                    start: 1,
                    count: 1,
                })?;
                match answer {
                    Some(Response::Property {
                        object: o,
                        property: p,
                        count,
                        data,
                        ..
                    }) if o == object && p == property => Ok((count > 0).then_some(data)),
                    other => Err(self.wrong_answer(other)),
                }
            }
            Resource::Memory(address) => self.read_memory(address, length).map(Some),
        }
    }

    /// Like [`Self::read_resource`], for reads whose failure does not matter: only a broken
    /// connection is an error.
    fn read_resource_leniently(
        &mut self,
        resource: Resource,
        length: usize,
    ) -> Result<Option<Vec<u8>>, PlanError> {
        match self.read_resource(resource, length) {
            Ok(data) => Ok(data),
            Err(error @ PlanError::Bus(_)) => Err(error),
            Err(_) => Ok(None),
        }
    }

    /// The serial number is read and kept; it is not compared with anything, and a device
    /// that does not give it is fine.
    fn read_serial_number(&mut self) -> Result<(), PlanError> {
        let Some(resource) = self.download.profile.serial_number else {
            return Ok(());
        };
        let name = &self.download.application_name;
        if NO_SERIAL_NUMBER
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            return Ok(());
        }
        self.serial_number = self.read_resource_leniently(resource, 6)?;
        if self.serial_number.is_none() {
            self.notes
                .push("the device did not give its serial number".into());
        }
        Ok(())
    }

    /// The identity in a program version answer: five bytes are manufacturer (2), application
    /// number (2) and version; four bytes have a one-byte manufacturer.
    fn application_from(data: &[u8]) -> Option<ApplicationId> {
        match *data {
            [m1, m2, n1, n2, version] => Some(ApplicationId {
                manufacturer: u16::from_be_bytes([m1, m2]),
                number: u16::from_be_bytes([n1, n2]),
                version,
            }),
            [manufacturer, n1, n2, version] => Some(ApplicationId {
                manufacturer: u16::from(manufacturer),
                number: u16::from_be_bytes([n1, n2]),
                version,
            }),
            _ => None,
        }
    }

    /// Before a partial download: does the device hold this application? Its load state must
    /// be "loaded", its identity the application's, and a second application must not be in
    /// the way. If not, a download with a fallback becomes the full one; one without stops.
    ///
    /// Reproduces the three reads of one recorded partial download (mask `0705`). The
    /// failing branches and the management style read on BCU 2 follow behaviour notes.
    fn check_partial_possible(&mut self) -> Result<(), PlanError> {
        let Some(problem) = self.partial_problem()? else {
            return Ok(());
        };
        match &self.download.fallback {
            Some(fallback) => {
                self.download = fallback;
                self.full_because = Some(problem);
                Ok(())
            }
            None => Err(PlanError::PartialNotPossible(problem)),
        }
    }

    fn partial_problem(&mut self) -> Result<Option<String>, PlanError> {
        let profile = self.download.profile;
        // Some devices are managed without load state machines and then have no load state.
        let mut state_machines = true;
        if let Some(address) = profile.management_style
            && let Some(style) = self.read_resource_leniently(Resource::Memory(address), 1)?
        {
            state_machines = style.first() == Some(&0);
        }
        if let Some(resource) = profile.application_load_status
            && (state_machines || !profile.load_status_needs_state_machines)
        {
            let status = self.read_resource(resource, 1)?;
            let Some(status) = status.as_deref().and_then(<[u8]>::first).copied() else {
                return Err(PlanError::ResourceNotFound(self.last_request()));
            };
            if status != state::LOADED {
                return Ok(Some(format!(
                    "the application program is not loaded (load state {status})"
                )));
            }
        }
        if let Some(resource) = profile.application_id {
            let Some(data) = self.read_resource(resource, 5)? else {
                return Err(PlanError::ResourceNotFound(self.last_request()));
            };
            let Some(found) = Self::application_from(&data) else {
                return Err(PlanError::Mismatch(self.last_request()));
            };
            self.device_application = Some(found);
            if found != self.download.application {
                return Ok(Some(format!(
                    "the device holds another application program: {found} instead of {}",
                    self.download.application
                )));
            }
        }
        // The device may have a second application program; this crate downloads none, so the
        // device should have none loaded either.
        if !self.download.ignore_second_application
            && let Some(resource) = profile.second_application_load_status
            && let Some(status) = self.read_resource_leniently(resource, 1)?
        {
            match status.first().copied() {
                Some(state::UNLOADED) | None => {}
                Some(state::LOADED) => self.notes.push(
                    "the device has a second application program loaded; it is left alone".into(),
                ),
                Some(status) => {
                    let empty = match profile.second_application_id {
                        Some(resource) => self
                            .read_resource(resource, 5)?
                            .is_some_and(|id| id.iter().all(|byte| *byte == 0)),
                        None => false,
                    };
                    if !empty {
                        return Ok(Some(format!(
                            "the device's second application program is not unloaded (load \
                             state {status})"
                        )));
                    }
                    self.notes.push(
                        "the second application program reports an odd load state but is empty"
                            .into(),
                    );
                }
            }
        }
        Ok(None)
    }

    /// The device must be of the application's manufacturer. A device that cannot be asked
    /// stops the run, too.
    ///
    /// The read is verified against two recorded downloads; stopping on a difference follows
    /// behaviour notes. Manufacturers that count as compatible with each other are not known
    /// to this crate: only the same identifier passes.
    fn check_manufacturer(&mut self) -> Result<(), PlanError> {
        let Some(resource) = self.download.profile.manufacturer_id else {
            return Ok(());
        };
        if self.download.skip_manufacturer_check {
            return Ok(());
        }
        let Some(data) = self.read_resource(resource, 2)? else {
            return Err(PlanError::ResourceNotFound(self.last_request()));
        };
        let actual = match *data.as_slice() {
            [single] => u16::from(single),
            [high, low, ..] => u16::from_be_bytes([high, low]),
            [] => return Err(PlanError::Mismatch(self.last_request())),
        };
        let expected = self.download.application.manufacturer;
        if actual != expected {
            return Err(PlanError::WrongManufacturer { expected, actual });
        }
        Ok(())
    }

    /// The device's hardware identification must fit what the image holds for it. A device
    /// that has no such resource passes, and so does an image that holds nothing to compare
    /// with (the read is made all the same).
    ///
    /// The read is verified against two recorded downloads, in which the comparison passes;
    /// stopping on a difference follows behaviour notes.
    fn check_hardware(&mut self) -> Result<(), PlanError> {
        let Some(check) = self.download.profile.hardware else {
            return Ok(());
        };
        let Some(device) = self.read_resource(check.read, usize::from(check.length))? else {
            return Ok(());
        };
        match &self.download.hardware {
            Some(expected) if !expected.accepts(&device) => Err(PlanError::WrongHardware {
                device,
                image: expected.data.clone(),
            }),
            _ => Ok(()),
        }
    }

    /// The caller's own safeguard: the device must not report another application. The
    /// identity is read here unless the pre-check has read it already.
    fn check_same_application(&mut self) -> Result<(), PlanError> {
        if self.device_application.is_none()
            && let Some(resource) = self.download.profile.application_id
            && let Some(data) = self.read_resource(resource, 5)?
        {
            self.device_application = Self::application_from(&data);
        }
        let expected = self.download.application;
        match self.device_application {
            Some(actual) if actual != expected => {
                Err(PlanError::WrongApplication { expected, actual })
            }
            _ => Ok(()),
        }
    }

    fn wrong_answer(&self, answer: Option<Response>) -> PlanError {
        let request = self.last_request();
        match answer {
            Some(response) => PlanError::UnexpectedResponse { request, response },
            None => PlanError::NoResponse(request),
        }
    }

    /// Before a partial download: read the block tables the project has checksums for and
    /// compare. Each eight-byte entry has the block size in bytes 0–3, "not checksummed" in
    /// bit 0 of byte 4 and the checksum in bytes 6–7.
    fn compare_stamp(&mut self) -> Result<(), PlanError> {
        let download = self.download;
        let Some(stamp) = download.stamp.as_ref().filter(|_| !download.kind.full) else {
            return Ok(());
        };
        let mut matches = true;
        for object in &stamp.objects {
            let count = u16::try_from(object.blocks.len())
                .map_err(|_| PlanError::Unsupported("oversized download stamp".into()))?;
            let data = match self.read_property(object.object, PID_MCB_TABLE, 1, count) {
                Ok(data) => data,
                Err(error @ PlanError::Bus(_)) => return Err(error),
                Err(_) => {
                    matches = false;
                    continue;
                }
            };
            let entries = data.chunks(8);
            if entries.len() != object.blocks.len() {
                matches = false;
            }
            for (entry, stored) in entries.zip(&object.blocks) {
                let same = match entry {
                    [_, _, _, _, flags, _, high, low] => {
                        (flags & 1 == 0) == stored.protected
                            && u16::from_be_bytes([*high, *low]) == stored.checksum
                    }
                    _ => false,
                };
                matches &= same;
            }
        }
        self.stamp_matches = Some(matches);
        Ok(())
    }

    // ---------------------------------------------------------------- properties

    /// Resolve a step's object and property to bus fields and the element size.
    fn property_target(&self, object: Target, property: u16) -> Result<(u8, u8, usize), PlanError> {
        let Target::Index(object) = object else {
            return Err(PlanError::Unsupported(
                "steps that address an object by type".into(),
            ));
        };
        let size = self
            .download
            .profile
            .property_element_size(object, property)
            .ok_or_else(|| {
                PlanError::Unsupported(format!(
                    "element size of property {property} of object {object} is unknown"
                ))
            })?;
        let property = u8::try_from(property)
            .map_err(|_| PlanError::Unsupported(format!("property identifier {property}")))?;
        Ok((object, property, size))
    }

    /// Elements of `size` bytes that fit one property frame.
    fn elements_per_frame(&self, size: usize) -> u16 {
        let room = usize::from(self.apdu_length).saturating_sub(5);
        (room / size.max(1)).clamp(1, 15) as u16
    }

    /// Read `count` elements from `start`, as many per request as fit a frame.
    fn read_property(
        &mut self,
        object: u8,
        property: u8,
        start: u16,
        count: u16,
    ) -> Result<Vec<u8>, PlanError> {
        self.ensure_connected()?;
        let size = self
            .download
            .profile
            .property_element_size(object, u16::from(property))
            .unwrap_or(1);
        let per_frame = self.elements_per_frame(size);
        let mut data = Vec::new();
        let mut done = 0;
        while done < count {
            let now = per_frame.min(count - done);
            let answer = self.send(Request::PropertyRead {
                object,
                property,
                start: start.saturating_add(done),
                count: now as u8,
            })?;
            match answer {
                Some(Response::Property {
                    object: o,
                    property: p,
                    count: answered,
                    data: bytes,
                    ..
                }) if o == object && p == property => {
                    if answered == 0 {
                        return Err(PlanError::ResourceNotFound(self.last_request()));
                    }
                    data.extend(bytes);
                }
                other => return Err(self.wrong_answer(other)),
            }
            done += now;
        }
        Ok(data)
    }

    fn write_property(
        &mut self,
        object: Target,
        property: u16,
        start: u16,
        count: u16,
        verify: bool,
        inline: &Option<Vec<u8>>,
    ) -> Result<(), PlanError> {
        let (object, property, size) = self.property_target(object, property)?;
        if start == 0 {
            return Err(PlanError::Unsupported(
                "writing a property's element count".into(),
            ));
        }
        // Inline data is cut to what the elements hold; shorter inline data is no data. The
        // image holds no property values, so there is no other source.
        let wanted = usize::from(count) * size;
        let Some(data) = inline.as_deref().and_then(|data| data.get(..wanted)) else {
            return Ok(());
        };
        self.ensure_connected()?;
        let per_frame = self.elements_per_frame(size);
        for (frame, chunk) in (0u16..).zip(data.chunks(usize::from(per_frame) * size)) {
            let answer = self.send(Request::PropertyWrite {
                object,
                property,
                start: start.saturating_add(frame.saturating_mul(per_frame)),
                count: (chunk.len() / size) as u8,
                data: chunk.to_vec(),
            })?;
            match answer {
                Some(Response::Property {
                    object: o,
                    property: p,
                    count: answered,
                    data: echoed,
                    ..
                }) if o == object && p == property => {
                    if answered == 0 {
                        return Err(PlanError::ResourceNotFound(self.last_request()));
                    }
                    if verify && echoed != chunk {
                        return Err(PlanError::Mismatch(self.last_request()));
                    }
                }
                other => return Err(self.wrong_answer(other)),
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn compare_property(
        &mut self,
        object: Target,
        property: u16,
        start: u16,
        count: u16,
        inline: &Option<Vec<u8>>,
        mask: &Option<Vec<u8>>,
        invert: bool,
    ) -> Result<(), PlanError> {
        let (object, property, size) = self.property_target(object, property)?;
        let wanted = usize::from(count) * size;
        // Without expected data, or with nothing left to compare, the step does nothing.
        let Some(expected) = inline.as_deref().and_then(|data| data.get(..wanted)) else {
            return Ok(());
        };
        let mask_at = |at: usize| match mask {
            Some(mask) => mask.get(at).copied().unwrap_or(0xFF),
            None => 0xFF,
        };
        if (0..wanted).all(|at| mask_at(at) == 0) {
            return Ok(());
        }
        let actual = self.read_property(object, property, start, count)?;
        let equal = actual.len() == expected.len()
            && actual
                .iter()
                .zip(expected)
                .enumerate()
                .all(|(at, (a, e))| a & mask_at(at) == e & mask_at(at));
        if equal == invert {
            return Err(PlanError::Mismatch(self.last_request()));
        }
        Ok(())
    }

    // ---------------------------------------------------------------- load state machines

    /// Send a load event with its nine bytes of additional information and check the state the
    /// machine ends up in.
    fn load_event(
        &mut self,
        target: Target,
        event: u8,
        info: [u8; 9],
        expected: u8,
    ) -> Result<(), PlanError> {
        let Target::Index(object) = target else {
            return Err(PlanError::Unsupported(
                "load state machines addressed by object type".into(),
            ));
        };
        self.ensure_connected()?;
        // States a device may report while it is still working on the event.
        let transitional = match event {
            event::LOAD_COMPLETED => Some(state::LOAD_COMPLETING),
            event::UNLOAD => Some(state::UNLOADING),
            _ => None,
        };
        let [first, rest @ ..] = info;
        let (mut actual, poll) = match self.download.profile.load_control {
            LoadControl::Property => {
                let mut data = vec![event];
                data.extend(info);
                let answer = self.send(Request::PropertyWrite {
                    object,
                    property: PID_LOAD_STATE_CONTROL,
                    start: 1,
                    count: 1,
                    data,
                })?;
                let poll = Request::PropertyRead {
                    object,
                    property: PID_LOAD_STATE_CONTROL,
                    start: 1,
                    count: 1,
                };
                (self.state_from(answer)?, poll)
            }
            LoadControl::Memory { control, status } => {
                if !(1..=4).contains(&object) {
                    return Err(PlanError::Unsupported(format!(
                        "load state machine {object} on a device with memory-mapped load control"
                    )));
                }
                // Record: state machine and event, first info byte, a zero, the other eight.
                let mut data = vec![(object << 4) | event, first, 0];
                data.extend(rest);
                self.send(Request::MemoryWrite {
                    address: control,
                    data,
                })?;
                let poll = Request::MemoryRead {
                    address: status + u16::from(object) - 1,
                    count: 1,
                };
                let answer = self.send(poll.clone())?;
                (self.state_from(answer)?, poll)
            }
        };
        let mut polls = 0;
        while Some(actual) == transitional && actual != expected {
            if polls >= self.options.max_state_polls {
                break;
            }
            polls += 1;
            let answer = self.send(poll.clone())?;
            actual = self.state_from(answer)?;
        }
        if actual != expected {
            return Err(PlanError::LoadState {
                object,
                expected,
                actual,
            });
        }
        Ok(())
    }

    /// The load state in an answer: its first data byte.
    /// Read the three load states the way a load event is followed up: the load state control
    /// property, or the status bytes of a device with memory-mapped load control. A state the
    /// device does not give is left out; only a broken connection is an error.
    fn read_load_states(&mut self) -> Result<(), PlanError> {
        let mut states = [None; 3];
        for (object, state) in (1u8..).zip(&mut states) {
            let poll = match self.download.profile.load_control {
                LoadControl::Property => Request::PropertyRead {
                    object,
                    property: PID_LOAD_STATE_CONTROL,
                    start: 1,
                    count: 1,
                },
                LoadControl::Memory { status, .. } => Request::MemoryRead {
                    address: status + u16::from(object) - 1,
                    count: 1,
                },
            };
            *state = match self.send(poll) {
                Ok(answer) => self.state_from(answer).ok(),
                Err(error @ PlanError::Bus(_)) => return Err(error),
                Err(_) => None,
            };
        }
        let [address_table, association_table, application] = states;
        self.load_states = Some(LoadStates {
            address_table,
            association_table,
            application,
        });
        Ok(())
    }

    fn state_from(&self, answer: Option<Response>) -> Result<u8, PlanError> {
        let first = match &answer {
            Some(Response::Property { data, .. } | Response::Memory { data, .. }) => {
                data.first().copied()
            }
            _ => None,
        };
        first.ok_or_else(|| self.wrong_answer(answer))
    }

    // ---------------------------------------------------------------- memory

    /// Bytes of data that fit one memory frame.
    fn memory_chunk(&self) -> usize {
        usize::from(self.apdu_length).saturating_sub(3).clamp(1, 63)
    }

    /// Write the image bytes to send within the range (or the inline data), in frames of
    /// ascending address; a gap in the bytes to send starts a new frame.
    fn write_memory(
        &mut self,
        address: u16,
        size: u16,
        verify: bool,
        inline: Option<&[u8]>,
    ) -> Result<(), PlanError> {
        let download = self.download;
        let runs: Vec<(u16, Vec<u8>)> = match inline.and_then(|data| data.get(..usize::from(size)))
        {
            Some(data) => vec![(address, data.to_vec())],
            None => download
                .image
                .runs(u32::from(address), u32::from(size))
                .into_iter()
                .filter_map(|run| Some((u16::try_from(run.address).ok()?, run.data)))
                .collect(),
        };
        if runs.is_empty() {
            return Ok(());
        }
        self.ensure_connected()?;
        // A verified write uses the device's echo mode where the mask has one (falling back to
        // write-and-read-back if the device will not switch it on), else it reads first.
        let echo_wanted = self
            .memory_auto_verify
            .unwrap_or(download.profile.verify_mode);
        let mut mode = if !verify {
            WriteMode::Plain
        } else if echo_wanted {
            if self.echo_mode()? {
                WriteMode::Echo
            } else {
                WriteMode::ReadBack
            }
        } else if self.optimistic_write {
            WriteMode::Optimistic
        } else {
            WriteMode::ReadBack
        };
        let reads_first = mode == WriteMode::Optimistic;
        let mut last_end: Option<u32> = None;
        for (start, data) in &runs {
            // After giving up on reading first, a long enough gap gives it another chance.
            if reads_first
                && mode == WriteMode::ReadBack
                && last_end.is_some_and(|end| u32::from(*start) >= end + 256)
            {
                mode = WriteMode::Optimistic;
            }
            if mode == WriteMode::Optimistic {
                if !self.write_optimistic(*start, data)? {
                    mode = WriteMode::ReadBack;
                }
            } else {
                self.write_run(*start, data, mode)?;
            }
            last_end = Some(u32::from(*start) + data.len() as u32);
        }
        Ok(())
    }

    fn write_run(&mut self, start: u16, data: &[u8], mode: WriteMode) -> Result<(), PlanError> {
        let mut address = start;
        for chunk in data.chunks(self.memory_chunk()) {
            let answer = self.send(Request::MemoryWrite {
                address,
                data: chunk.to_vec(),
            })?;
            match mode {
                WriteMode::Echo => match answer {
                    Some(Response::Memory {
                        address: echoed,
                        data,
                    }) if echoed == address && data == chunk => {}
                    Some(Response::Memory { .. }) => {
                        return Err(PlanError::Mismatch(self.last_request()));
                    }
                    other => return Err(self.wrong_answer(other)),
                },
                WriteMode::ReadBack => {
                    if self.read_memory(address, chunk.len())? != chunk {
                        return Err(PlanError::Mismatch(self.last_request()));
                    }
                }
                WriteMode::Plain | WriteMode::Optimistic => {}
            }
            address = address.wrapping_add(chunk.len() as u16);
        }
        Ok(())
    }

    fn read_memory(&mut self, address: u16, count: usize) -> Result<Vec<u8>, PlanError> {
        let answer = self.send(Request::MemoryRead {
            address,
            count: count as u8,
        })?;
        match answer {
            Some(Response::Memory {
                address: answered,
                data,
            }) if answered == address && data.len() == count => Ok(data),
            other => Err(self.wrong_answer(other)),
        }
    }

    /// Read each frame-sized piece first and write only the stretch from its first to its last
    /// differing byte, then read that stretch back. Returns `false` after giving up: the rest
    /// of the run has then been written and read back piece by piece.
    ///
    /// The read-compare-write-read-back cycle is verified against a recorded download. Giving
    /// up (when, after at least 32 bytes, mismatches outnumber half the matches, or after four
    /// pieces in a row with a mismatch) follows behaviour notes.
    fn write_optimistic(&mut self, start: u16, data: &[u8]) -> Result<bool, PlanError> {
        let (mut matched, mut mismatched, mut in_a_row) = (0usize, 0usize, 0usize);
        let mut address = start;
        let mut rest = data;
        while !rest.is_empty() {
            if (matched + mismatched >= 32 && 2 * mismatched > matched) || in_a_row >= 4 {
                self.write_run(address, rest, WriteMode::ReadBack)?;
                return Ok(false);
            }
            let (chunk, tail) = rest.split_at(self.memory_chunk().min(rest.len()));
            let present = self.read_memory(address, chunk.len())?;
            let differing: Vec<usize> = (0..chunk.len())
                .filter(|at| chunk.get(*at) != present.get(*at))
                .collect();
            matched += chunk.len() - differing.len();
            mismatched += differing.len();
            match (differing.first(), differing.last()) {
                (Some(first), Some(last)) => {
                    in_a_row += 1;
                    let span = chunk.get(*first..=*last).unwrap_or_default();
                    self.write_run(
                        address.wrapping_add(*first as u16),
                        span,
                        WriteMode::ReadBack,
                    )?;
                }
                _ => in_a_row = 0,
            }
            address = address.wrapping_add(chunk.len() as u16);
            rest = tail;
        }
        Ok(true)
    }

    /// Make the device echo memory writes: read the device control property, and if its
    /// verify-mode bit is clear, read it once more, write it back with the bit set and read it
    /// again. Returns `false` when the property cannot be read.
    ///
    /// The read, read, write, read sequence is what two recorded downloads show when the bit
    /// starts out clear. That nothing is written when it is already set follows behaviour notes.
    fn echo_mode(&mut self) -> Result<bool, PlanError> {
        if self.echo_unavailable {
            return Ok(false);
        }
        let control = match self.device_control {
            Some(value) => value,
            None => match self.read_device_control()? {
                Some(value) => value,
                None => {
                    self.echo_unavailable = true;
                    return Ok(false);
                }
            },
        };
        if control & VERIFY_MODE != 0 {
            self.device_control = Some(control);
            return Ok(true);
        }
        let current = self.read_device_control()?.unwrap_or(control);
        self.send(Request::PropertyWrite {
            object: 0,
            property: PID_DEVICE_CONTROL,
            start: 1,
            count: 1,
            data: vec![current | VERIFY_MODE],
        })?;
        match self.read_device_control()? {
            Some(value) if value & VERIFY_MODE != 0 => {
                self.device_control = Some(value);
                Ok(true)
            }
            _ => Err(PlanError::Mismatch(self.last_request())),
        }
    }

    fn read_device_control(&mut self) -> Result<Option<u8>, PlanError> {
        let answer = self.send(Request::PropertyRead {
            object: 0,
            property: PID_DEVICE_CONTROL,
            start: 1,
            count: 1,
        })?;
        Ok(match answer {
            Some(Response::Property { count, data, .. }) if count > 0 => data.first().copied(),
            _ => None,
        })
    }
}
