//! The two things done to a device over a bus: comparing its memory with the plan, and the
//! download itself.
//!
//! The planner of `knxide-program` is synchronous: it asks a device and waits for the answer.
//! Here it runs on a blocking thread and its device is a pair of channels; the other ends are
//! served on the runtime by the [`Session`] that owns the transport connection. The planner
//! decides what is sent next; nothing in this module does.
//!
//! The two operations treat trouble on the bus differently. A comparison only reads, so
//! whatever goes wrong it opens a new connection and reads again, and when it has to give up
//! it still says what it found. A download never starts over on its own: it stops at the first
//! thing that is not as expected and says how far it got.

use std::sync::Arc;
use std::time::Duration;

use knxide_program::compare;
use knxide_program::mask::LoadControl;
use knxide_program::{
    Device, Download, Expect, Fault, Plan, PlanError, PlanFailure, PlanOptions, Progress, Request,
    Response,
};
use tokio::sync::{mpsc, watch};

use crate::link::Link;
use crate::session::{Session, SessionFault};

/// Bytes read per request when comparing. With an APDU length of 15 a memory read returns
/// 12 bytes at most.
pub const COMPARE_CHUNK: usize = 12;

/// How persistent a comparison is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompareLimits {
    /// How often one read (or the pre-check) may fail in a row before the comparison gives up.
    pub attempts: u32,
    /// How many new connections a comparison opens in all before it gives up.
    pub reconnects: u32,
    /// How long to wait between closing a connection and opening the next.
    pub reconnect_pause: Duration,
    /// After how long a silence from the device a connection is given up for a new one,
    /// instead of repeating into it. A device drops a connection idle for about six seconds.
    pub stale_after: Option<Duration>,
    /// When the repetition of a read is acknowledged and no answer follows within this time,
    /// the answer is taken as lost with the acknowledgement of the first transmission: it is
    /// acknowledged unseen and the read is made again on the same connection, instead of
    /// waiting for the device to close it. `None` leaves such a read to the new connection
    /// that follows. See [`knxide_program::transport`]: this rests on an inference of medium
    /// confidence from two observations on one device of mask 0021.
    pub lost_answer_after: Option<Duration>,
}

impl Default for CompareLimits {
    fn default() -> Self {
        Self {
            attempts: 5,
            reconnects: 60,
            reconnect_pause: Duration::from_millis(250),
            stale_after: Some(Duration::from_secs(6)),
            lost_answer_after: Some(Duration::from_secs(1)),
        }
    }
}

/// How a download deals with an exchange that fails: see [`download`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadLimits {
    /// How many failed exchanges a download may recover from; the next one stops it. With 0 a
    /// download stops at the first exchange it cannot complete, and none of the waits below
    /// apply.
    pub recoveries: u32,
    /// When the repetition of a request is acknowledged and no answer follows within this
    /// time, the answer is taken as lost and acknowledged unseen, and the recovery reads what
    /// the device holds.
    pub lost_answer_after: Option<Duration>,
    /// The same for a request acknowledged at the first go. Longer, so that a device that
    /// repeats its answer after three seconds has done so, and well short of the six seconds
    /// after which a device gives a connection up.
    pub unanswered_after: Option<Duration>,
    /// How long to wait between closing a connection and opening the next.
    pub reconnect_pause: Duration,
}

impl Default for DownloadLimits {
    fn default() -> Self {
        Self {
            recoveries: 3,
            lost_answer_after: Some(Duration::from_secs(1)),
            unanswered_after: Some(Duration::from_secs(4)),
            reconnect_pause: Duration::from_millis(250),
        }
    }
}

impl DownloadLimits {
    /// No recovery at all: the download stops at the first exchange that fails.
    pub fn none() -> Self {
        Self {
            recoveries: 0,
            lost_answer_after: None,
            unanswered_after: None,
            reconnect_pause: Duration::from_millis(250),
        }
    }
}

/// Something to tell whoever watches an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// A request goes to the device.
    Request(Request),
    /// The device's answer to the last request.
    Response(Response),
    /// The last request should have been answered and was not.
    NoResponse,
    /// Frames of the last request had to be sent again, this many times.
    Repeated(u32),
    /// A request was made again on the same connection, its answer having been lost, this
    /// many times. Comparisons only.
    AskedAgain(u32),
    /// A new connection is opened in place of the one that failed.
    Reconnecting,
    /// How many requests the operation now expects to send in all, those sent included.
    Expected(usize),
    /// A download recovers from an exchange that failed: it reads what the device holds
    /// instead of stopping. The string says what failed.
    Recovering(String),
    /// The operation enters another part, named in words: "Address table", "Restart".
    Phase(&'static str),
    /// The planner waits before its next request.
    Pause(Duration),
    /// Something happened below the level of requests, or is worth knowing.
    Note(String),
}

/// Where reports go.
pub type Reporter<'r> = &'r mut (dyn FnMut(Report) + Send);

/// Why an operation stopped without a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// What went wrong, for a person to read.
    pub message: String,
    /// The check of the pre-check that stopped the operation, if it was one: `mask`,
    /// `access`, `manufacturer`, `hardware`, `application` or `partial`.
    pub check: Option<&'static str>,
    /// Whether the operation was cancelled.
    pub cancelled: bool,
    /// How many requests had gone out.
    pub requests: usize,
    /// How far a download got: the steps completed, the step under way and the last write
    /// the device took. `None` for a comparison, which writes nothing.
    pub progress: Option<Box<Progress>>,
}

impl Failure {
    /// A failure that is no check and no cancellation.
    pub fn plain(message: String, requests: usize) -> Self {
        Self {
            message,
            check: None,
            cancelled: false,
            requests,
            progress: None,
        }
    }

    /// What the device has been told, in a sentence: for a download how far it got, for a
    /// comparison that nothing was written.
    pub fn written(&self) -> String {
        match self.progress.as_deref() {
            Some(progress) if progress.last_write.is_some() => format!(
                "{progress}. The device is partly loaded and needs a complete download, here \
                 or in ETS"
            ),
            _ => "nothing was written to the device".into(),
        }
    }
}

/// The check a planning error belongs to, if it comes from the pre-check.
fn check_of(error: &PlanError) -> Option<&'static str> {
    match error {
        PlanError::WrongMask { .. } => Some("mask"),
        PlanError::AccessDenied { .. } => Some("access"),
        PlanError::WrongManufacturer { .. } => Some("manufacturer"),
        PlanError::WrongHardware { .. } => Some("hardware"),
        PlanError::WrongApplication { .. } => Some("application"),
        PlanError::PartialNotPossible(_) => Some("partial"),
        _ => None,
    }
}

const CANCELLED: &str = "cancelled";

impl From<PlanFailure> for Failure {
    fn from(failure: PlanFailure) -> Self {
        let cancelled = failure.error == PlanError::Bus(CANCELLED.into());
        let message = match &failure.error {
            PlanError::WrongMask { .. } => format!("mask check failed: {}", failure.error),
            PlanError::AccessDenied { .. } => format!("access check failed: {}", failure.error),
            _ if cancelled => "cancelled".into(),
            other => other.to_string(),
        };
        Self {
            message,
            check: check_of(&failure.error),
            cancelled,
            requests: failure.requests.len(),
            progress: Some(failure.progress),
        }
    }
}

/// Lets an operation be stopped from outside.
#[derive(Debug, Clone)]
pub struct Cancel(watch::Receiver<bool>);

/// The switch that goes with a [`Cancel`].
#[derive(Debug)]
pub struct CancelSwitch(watch::Sender<bool>);

impl Cancel {
    /// A pair: the switch, and what an operation watches.
    pub fn new() -> (CancelSwitch, Self) {
        let (sender, receiver) = watch::channel(false);
        (CancelSwitch(sender), Self(receiver))
    }

    /// Something that never cancels.
    pub fn never() -> Self {
        Self::new().1
    }

    fn is_set(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves when the operation is cancelled; never, if it is not.
    async fn set(&mut self) {
        if self.0.wait_for(|cancelled| *cancelled).await.is_err() {
            // The switch is gone: nobody can cancel any more.
            std::future::pending::<()>().await;
        }
    }
}

impl CancelSwitch {
    /// Stop the operation at its next request.
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
}

/// What can keep a request from going through.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Trouble {
    /// The operation was cancelled.
    Cancelled,
    /// The access to the bus is gone.
    Link(String),
    /// The connection to the device failed; a new one may work.
    Device(String),
    /// The device answered, and its answer is that it does not give what was asked for.
    Refused,
    /// The device acknowledged the request and its answer did not arrive; the connection is
    /// still open.
    AnswerLost(String),
}

impl From<SessionFault> for Trouble {
    fn from(fault: SessionFault) -> Self {
        match fault {
            SessionFault::Device(what) => Self::Device(what),
            SessionFault::Link(what) => Self::Link(what),
            SessionFault::AnswerLost(what) => Self::AnswerLost(what),
        }
    }
}

impl From<Trouble> for Fault {
    fn from(trouble: Trouble) -> Self {
        Self(match trouble {
            Trouble::Cancelled => CANCELLED.into(),
            Trouble::Link(what) | Trouble::Device(what) | Trouble::AnswerLost(what) => what,
            Trouble::Refused => "the device declined the request".into(),
        })
    }
}

/// What an operation has cost so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Tally {
    requests: usize,
    repetitions: u32,
    re_requests: u32,
    recoveries: u32,
}

enum Command {
    Exchange(Request, Expect),
    Pause(Duration),
    /// The planner enters another part. Nothing is answered.
    Phase(&'static str),
}

/// The planner's device: hands every request to the runtime and blocks for the answer.
struct Bridge {
    commands: mpsc::Sender<Command>,
    replies: std::sync::mpsc::Receiver<Result<Option<Response>, Fault>>,
}

impl Bridge {
    fn ask(&mut self, command: Command) -> Result<Option<Response>, Fault> {
        let gone = || Fault("the operation ended".into());
        self.commands.blocking_send(command).map_err(|_| gone())?;
        self.replies.recv().map_err(|_| gone())?
    }
}

impl Device for Bridge {
    fn exchange(&mut self, request: &Request, expect: Expect) -> Result<Option<Response>, Fault> {
        self.ask(Command::Exchange(request.clone(), expect))
    }

    fn pause(&mut self, duration: Duration) -> Result<(), Fault> {
        self.ask(Command::Pause(duration)).map(drop)
    }

    fn phase(&mut self, name: &'static str) {
        let _ = self.commands.blocking_send(Command::Phase(name));
    }
}

/// One request through the session, with reports and cancellation.
async fn exchange<L: Link>(
    session: &mut Session<L>,
    request: &Request,
    expect: Expect,
    report: Reporter<'_>,
    cancel: &mut Cancel,
    tally: &mut Tally,
) -> Result<Option<Response>, Trouble> {
    if cancel.is_set() {
        return Err(Trouble::Cancelled);
    }
    tally.requests += 1;
    report(Report::Request(request.clone()));
    let result = tokio::select! {
        result = session.exchange(request, expect) => result.map_err(Trouble::from),
        () = cancel.set() => Err(Trouble::Cancelled),
    };
    let repeated = session.take_repetitions();
    if repeated > 0 {
        tally.repetitions += repeated;
        report(Report::Repeated(repeated));
    }
    let asked_again = session.take_re_requests();
    if asked_again > 0 {
        tally.re_requests += asked_again;
        report(Report::AskedAgain(asked_again));
    }
    for note in session.take_notes() {
        report(Report::Note(note));
    }
    match &result {
        Ok(Some(response)) => report(Report::Response(response.clone())),
        Ok(None) if expect == Expect::Answer => report(Report::NoResponse),
        _ => {}
    }
    result
}

/// Run `work`, which plans against a device, on a blocking thread, and serve its device
/// from `session`. Returns what `work` returns and the trouble that ended it, if any did.
async fn drive<L: Link, T: Send + 'static>(
    session: &mut Session<L>,
    report: Reporter<'_>,
    cancel: &mut Cancel,
    tally: &mut Tally,
    mut recovery: Option<&mut Recovery>,
    work: impl FnOnce(&mut dyn Device) -> T + Send + 'static,
) -> Result<(T, Option<Trouble>), String> {
    let (commands, mut asked) = mpsc::channel(1);
    let (answers, replies) = std::sync::mpsc::channel();
    let worker = tokio::task::spawn_blocking(move || work(&mut Bridge { commands, replies }));
    let mut trouble = None;
    while let Some(command) = asked.recv().await {
        let reply = match command {
            Command::Phase(name) => {
                report(Report::Phase(name));
                continue;
            }
            Command::Exchange(request, expect) => {
                let mut reply = exchange(session, &request, expect, report, cancel, tally).await;
                if let Some(recovery) = recovery.as_deref_mut() {
                    if let Err(trouble @ (Trouble::Device(_) | Trouble::AnswerLost(_))) = &reply
                        && recovery.used < recovery.limits.recoveries
                        && recovery.covers(&request)
                    {
                        recovery.used += 1;
                        tally.recoveries += 1;
                        let trouble = trouble.clone();
                        reply = recover(
                            session, &request, expect, trouble, recovery, report, cancel, tally,
                        )
                        .await;
                    }
                    recovery.follow(&request, &reply);
                }
                reply
            }
            Command::Pause(duration) => {
                report(Report::Pause(duration));
                tokio::select! {
                    result = session.pause(duration) => {
                        result.map(|()| None).map_err(Trouble::from)
                    }
                    () = cancel.set() => Err(Trouble::Cancelled),
                }
            }
        };
        if let Err(error) = &reply {
            trouble = Some(error.clone());
        }
        if answers.send(reply.map_err(Fault::from)).is_err() {
            break;
        }
    }
    let result = worker
        .await
        .map_err(|error| format!("the planner stopped unexpectedly: {error}"))?;
    Ok((result, trouble))
}

/// The outcome of a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// What was sent, and what the pre-check found.
    pub plan: Plan,
    /// How many frames had to be sent again.
    pub repetitions: u32,
    /// How many exchanges failed and were recovered from: see [`download`].
    pub recoveries: u32,
}

/// Property of an interface object that takes load events and gives the load state.
const LOAD_STATE_CONTROL: u8 = 5;
/// Property of the device object that holds, among other things, whether memory writes are
/// echoed. It is set for a connection.
const DEVICE_CONTROL: (u8, u8) = (0, 14);

/// What a download keeps to be able to recover from an exchange that failed.
struct Recovery {
    limits: DownloadLimits,
    load_control: LoadControl,
    mask_version: u16,
    key: u32,
    /// Recoveries so far.
    used: u32,
    /// What the download last wrote to the device control property on this connection: put
    /// back on a new connection, where a device may have dropped it.
    device_control: Option<Vec<u8>>,
}

/// What a write should have left in the device, to be found by reading.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Check {
    /// Memory holding the data written.
    Memory { address: u16, data: Vec<u8> },
    /// A property holding the data written.
    Property {
        object: u8,
        property: u8,
        start: u16,
        count: u8,
        data: Vec<u8>,
    },
    /// A load state machine in the state the event leads to.
    LoadEvent { event: u8, read: Request },
}

/// What the reading says about a write whose outcome was not seen.
enum Verdict {
    /// The device holds what the write should have produced; this is the answer the write
    /// would have had.
    Taken(Option<Response>),
    /// The device never acknowledged the write and does not hold what it writes.
    NotTaken,
    /// Neither: the string says what was found.
    Differs(String),
}

impl Check {
    fn of(request: &Request, load_control: LoadControl) -> Option<Self> {
        match request {
            Request::MemoryWrite { address, data } => match (load_control, data.first()) {
                (LoadControl::Memory { control, status }, Some(head))
                    if control == *address && head >> 4 >= 1 =>
                {
                    Some(Self::LoadEvent {
                        event: head & 0x0F,
                        read: Request::MemoryRead {
                            address: status + u16::from(head >> 4) - 1,
                            count: 1,
                        },
                    })
                }
                _ => Some(Self::Memory {
                    address: *address,
                    data: data.clone(),
                }),
            },
            Request::PropertyWrite {
                object,
                property,
                start,
                count,
                data,
            } => match data.first() {
                Some(event)
                    if *property == LOAD_STATE_CONTROL
                        && data.len() == 10
                        && load_control == LoadControl::Property =>
                {
                    Some(Self::LoadEvent {
                        event: *event,
                        read: Request::PropertyRead {
                            object: *object,
                            property: LOAD_STATE_CONTROL,
                            start: 1,
                            count: 1,
                        },
                    })
                }
                _ => Some(Self::Property {
                    object: *object,
                    property: *property,
                    start: *start,
                    count: *count,
                    data: data.clone(),
                }),
            },
            _ => None,
        }
    }

    /// The read that shows what the device holds.
    fn read(&self) -> Request {
        match self {
            Self::Memory { address, data } => Request::MemoryRead {
                address: *address,
                count: data.len() as u8,
            },
            Self::Property {
                object,
                property,
                start,
                count,
                ..
            } => Request::PropertyRead {
                object: *object,
                property: *property,
                start: *start,
                count: *count,
            },
            Self::LoadEvent { read, .. } => read.clone(),
        }
    }

    /// Judge `seen`, the answer to [`Check::read`]. `acknowledged` says whether the device
    /// is known to have taken the write; `answered` whether the write has an answer.
    fn verdict(&self, seen: Option<&Response>, acknowledged: bool, answered: bool) -> Verdict {
        let held = match seen {
            Some(Response::Memory { data, .. }) if !data.is_empty() => data,
            Some(Response::Property { count, data, .. }) if *count > 0 => data,
            Some(other) => return Verdict::Differs(format!("the device answered {other}")),
            None => return Verdict::Differs("the device gave no answer".into()),
        };
        match self {
            Self::Memory { address, data } => {
                if held == data {
                    Verdict::Taken(answered.then(|| Response::Memory {
                        address: *address,
                        data: data.clone(),
                    }))
                } else if !acknowledged {
                    Verdict::NotTaken
                } else {
                    Verdict::Differs(format!(
                        "the device acknowledged the write and its memory at {address:04X} \
                         does not hold what was written"
                    ))
                }
            }
            Self::Property {
                object,
                property,
                start,
                count,
                data,
            } => {
                if held == data {
                    Verdict::Taken(answered.then(|| Response::Property {
                        object: *object,
                        property: *property,
                        start: *start,
                        count: *count,
                        data: data.clone(),
                    }))
                } else if !acknowledged {
                    Verdict::NotTaken
                } else {
                    Verdict::Differs(format!(
                        "the device acknowledged the write and property {property} of object \
                         {object} does not hold what was written"
                    ))
                }
            }
            Self::LoadEvent { event, read } => {
                let Some(state) = held.first().copied() else {
                    return Verdict::Differs("the device gave no load state".into());
                };
                // Where each event leads: load to loading; load completed to loaded, by way of
                // "load completing"; unload to unloaded, by way of "unloading". Every other
                // event leaves a state machine that is loading in that state, so the state
                // cannot tell whether the event was taken.
                let (leads_to, telling): (&[u8], bool) = match event {
                    1 => (&[2], true),
                    2 => (&[1, 5], true),
                    4 => (&[0, 4], true),
                    _ => (&[2], false),
                };
                if leads_to.contains(&state) && (telling || acknowledged) {
                    // The state is what the planner goes by; it gets it as the answer.
                    let answer = match (answered, read) {
                        (
                            true,
                            Request::PropertyRead {
                                object,
                                property,
                                start,
                                count,
                            },
                        ) => Some(Response::Property {
                            object: *object,
                            property: *property,
                            start: *start,
                            count: *count,
                            data: vec![state],
                        }),
                        _ => None,
                    };
                    Verdict::Taken(answer)
                } else if !telling {
                    Verdict::Differs(format!(
                        "the load state ({state}) cannot tell whether the device took the event"
                    ))
                } else if !acknowledged && state != 3 {
                    Verdict::NotTaken
                } else {
                    Verdict::Differs(format!(
                        "the load state is {state}, not what the event leads to"
                    ))
                }
            }
        }
    }
}

impl Recovery {
    fn new(download: &Download, options: &PlanOptions, limits: DownloadLimits) -> Self {
        Self {
            limits,
            load_control: download.profile.load_control,
            mask_version: download.profile.mask_version,
            key: options.key,
            used: 0,
            device_control: None,
        }
    }

    /// Whether a failed `request` can be recovered from: a read is asked again, a write is
    /// looked up in the device. A restart, a connect and a disconnect are neither.
    fn covers(&self, request: &Request) -> bool {
        !matches!(
            request,
            Request::Connect | Request::Disconnect | Request::Restart
        )
    }

    /// Keep track of what is set for this connection only.
    fn follow(&mut self, request: &Request, reply: &Result<Option<Response>, Trouble>) {
        match request {
            Request::Connect | Request::Restart => self.device_control = None,
            Request::PropertyWrite {
                object,
                property,
                data,
                ..
            } if (*object, *property) == DEVICE_CONTROL && reply.is_ok() => {
                self.device_control = Some(data.clone());
            }
            _ => {}
        }
    }
}

/// A new connection for a download that lost its own: the opening as at the start, and the
/// device control property as the download had set it.
async fn reopen<L: Link>(
    session: &mut Session<L>,
    recovery: &Recovery,
    report: Reporter<'_>,
    cancel: &mut Cancel,
    tally: &mut Tally,
) -> Result<(), Trouble> {
    report(Report::Reconnecting);
    reconnect(
        session,
        recovery.mask_version,
        recovery.key,
        recovery.limits.reconnect_pause,
        report,
        cancel,
        tally,
    )
    .await?;
    let Some(wanted) = &recovery.device_control else {
        return Ok(());
    };
    let (object, property) = DEVICE_CONTROL;
    let read = Request::PropertyRead {
        object,
        property,
        start: 1,
        count: 1,
    };
    match exchange(session, &read, Expect::Answer, report, cancel, tally).await? {
        Some(Response::Property { data, count, .. }) if count > 0 && data == *wanted => Ok(()),
        Some(Response::Property { count, .. }) if count > 0 => {
            // The setting went with the old connection. Writing it is a write whose outcome
            // is seen in its answer.
            let write = Request::PropertyWrite {
                object,
                property,
                start: 1,
                count: 1,
                data: wanted.clone(),
            };
            match exchange(session, &write, Expect::Answer, report, cancel, tally).await? {
                Some(Response::Property { data, .. }) if data == *wanted => Ok(()),
                _ => Err(Trouble::Device(
                    "the device control setting could not be put back on the new connection".into(),
                )),
            }
        }
        _ => Err(Trouble::Device(
            "the device control setting could not be read on the new connection".into(),
        )),
    }
}

/// Recover from the failed exchange of `request`, without ever sending a write on a guess.
///
/// A read is asked again. For a write, what the device holds is read: the memory written,
/// the property written, the load state an event leads to. When that is what the write
/// should have produced, the download goes on as if the write's answer had arrived. When it
/// is not, and the device never acknowledged the write, the write is sent again; in every
/// other case the download stops. The reading is done on the connection at hand when the
/// answer was only lost (it has been acknowledged unseen), else on a new connection.
#[allow(clippy::too_many_arguments)]
async fn recover<L: Link>(
    session: &mut Session<L>,
    request: &Request,
    expect: Expect,
    trouble: Trouble,
    recovery: &mut Recovery,
    report: Reporter<'_>,
    cancel: &mut Cancel,
    tally: &mut Tally,
) -> Result<Option<Response>, Trouble> {
    let acknowledged = session.was_acknowledged();
    let failed = Fault::from(trouble.clone()).0;
    report(Report::Recovering(failed.clone()));
    // Whatever goes wrong from here on, the download stops with the first failure named.
    let stop = |why: String| Trouble::Device(format!("{failed}; recovering from that, {why}"));
    let passing = |trouble: Trouble| match trouble {
        Trouble::Cancelled | Trouble::Link(_) => trouble,
        other => stop(Fault::from(other).0),
    };
    let mut fresh = false;
    if !(matches!(trouble, Trouble::AnswerLost(_)) && session.is_open()) {
        reopen(session, recovery, report, cancel, tally)
            .await
            .map_err(passing)?;
        fresh = true;
    }

    let check = Check::of(request, recovery.load_control);
    // A read is simply asked again; for a write, the read that shows what it left.
    let (asked, wanted) = match &check {
        Some(check) => (check.read(), Expect::Answer),
        None => (request.clone(), expect),
    };
    let mut seen = exchange(session, &asked, wanted, report, cancel, tally).await;
    if matches!(seen, Err(Trouble::Device(_) | Trouble::AnswerLost(_))) && !fresh {
        // The connection at hand was not usable after all.
        reopen(session, recovery, report, cancel, tally)
            .await
            .map_err(passing)?;
        seen = exchange(session, &asked, wanted, report, cancel, tally).await;
    }
    let seen = seen.map_err(passing)?;
    let Some(check) = check else {
        report(Report::Note(format!("{request}: asked again and answered")));
        return Ok(seen);
    };
    match check.verdict(seen.as_ref(), acknowledged, expect == Expect::Answer) {
        Verdict::Taken(answer) => {
            report(Report::Note(format!(
                "{request}: the device holds what this writes; going on"
            )));
            Ok(answer)
        }
        Verdict::NotTaken => {
            report(Report::Note(format!(
                "{request}: the device never acknowledged this and does not hold what it \
                 writes; sending it again"
            )));
            exchange(session, request, expect, report, cancel, tally)
                .await
                .map_err(passing)
        }
        Verdict::Differs(why) => Err(stop(why)),
    }
}

/// Download `download` to the device behind `session`: the opening, the pre-check and the
/// procedure, each request decided by the planner from the answers before it.
///
/// It stops at the first answer that is not the expected one. The failure says how far the
/// download got. The connection is left as the planner left it; the caller closes the
/// session.
///
/// # An exchange that fails
///
/// A write is never sent a second time on a guess about what the device made of the first.
/// Up to [`DownloadLimits::recoveries`] times, a download does this instead of stopping:
///
/// - A read is asked again.
/// - For a write, it reads what the write should have left: the memory or the property
///   written, or the load state the event leads to. If the device holds that, the download
///   goes on. If it does not and the device never acknowledged the write, the write is sent
///   again. Otherwise the download stops, as it does for an event whose effect the load state
///   cannot show and that was never acknowledged.
/// - The reading is done on the same connection when the device acknowledged the request and
///   only its answer went missing (the answer is then acknowledged unseen, see
///   [`knxide_program::transport`]); else on a new connection, opened as at the start, on
///   which the device control setting of the download is put back if the device dropped it.
///
/// **This recovery is this tool's own.** It is not taken from behaviour notes on any other
/// tool, and it has only been run against simulated devices, never against a real one.
pub async fn download<L: Link>(
    session: &mut Session<L>,
    download: Arc<Download>,
    options: PlanOptions,
    limits: DownloadLimits,
    report: Reporter<'_>,
    cancel: &mut Cancel,
) -> Result<Downloaded, Failure> {
    session.set_stale_after(None);
    let mut recovery = Recovery::new(&download, &options, limits);
    if limits.recoveries > 0 {
        session.set_lost_answer_after(limits.lost_answer_after);
        session.set_unanswered_after(limits.unanswered_after);
        // What the device made of a request is looked up, not tried out.
        session.set_ask_again(false);
    }
    let mut tally = Tally::default();
    let (planned, _) = drive(
        session,
        report,
        cancel,
        &mut tally,
        Some(&mut recovery),
        move |device| download.plan(device, &options),
    )
    .await
    .map_err(|message| Failure::plain(message, tally.requests))?;
    match planned {
        Ok(plan) => Ok(Downloaded {
            plan,
            repetitions: tally.repetitions,
            recoveries: tally.recoveries,
        }),
        Err(failure) => Err(Failure::from(failure)),
    }
}

/// A stretch of memory where the device holds something else than the plan would write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// Address of the first differing byte.
    pub address: u16,
    /// What the download would write.
    pub planned: Vec<u8>,
    /// What the device holds.
    pub device: Vec<u8>,
}

/// A stretch of memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stretch {
    /// Address of the first byte.
    pub address: u16,
    /// Number of bytes.
    pub length: usize,
}

/// The result of comparing a device with a plan. It may be partial: when the comparison had
/// to give up, [`Comparison::unread`] lists what it did not get to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// What the opening and the pre-check found; its kind says which download would run.
    pub found: Plan,
    /// Bytes the download would send.
    pub planned: usize,
    /// Bytes read and compared.
    pub compared: usize,
    /// Bytes that are in the device as planned.
    pub matching: usize,
    /// Bytes that differ.
    pub differing: usize,
    /// The differing stretches, ascending.
    pub differences: Vec<Difference>,
    /// What could not be read, ascending; empty when the comparison is complete.
    pub unread: Vec<Stretch>,
    /// What was read where the download sends nothing: parts of the device's group address
    /// and association tables, where those have another shape than the plan's.
    pub beside: Vec<compare::Difference>,
    /// Why the comparison stopped early, if it did.
    pub stopped_because: Option<String>,
    /// Whether the comparison stopped because the device declined to give its memory (a read
    /// answered with no data), not because of trouble on the way.
    pub refused: bool,
    /// For a device that is unloaded or only partly loaded: what it lacks, in a sentence.
    /// Its memory was not read then; there is nothing to compare, and it needs a full
    /// download.
    pub not_loaded: Option<String>,
    /// How many frames had to be sent again.
    pub repetitions: u32,
    /// How often a read was made again on the same connection because its answer was lost.
    pub re_requests: u32,
    /// How often a new connection was opened after one failed.
    pub reconnects: u32,
    /// Requests sent in all.
    pub requests: usize,
}

impl Comparison {
    /// Whether every byte the download would send was read.
    pub fn is_complete(&self) -> bool {
        self.unread.is_empty()
    }
}

/// Whether a planning failure is trouble with the connection, which a new connection may
/// cure, and not something the device said.
fn worth_another_connection(error: &PlanError) -> bool {
    matches!(
        error,
        PlanError::Bus(_) | PlanError::NoResponse(_) | PlanError::UnexpectedResponse { .. }
    )
}

/// Compare the device behind `session` with what `download` would write. Nothing is written.
///
/// First the pre-check of a download is run (it only reads, and it stops for the same reasons
/// a download would), then every byte the download would send is read, twelve at a time.
///
/// Reading is safe to repeat, so trouble with the connection never ends the comparison by
/// itself. When a read fails for whatever reason (no acknowledgement after the repetitions, no
/// answer, a foreign answer, the device closing the connection, a connection gone silent)
/// the connection is closed, a new one is opened as at the start (connect, device descriptor,
/// access key) and the read is made again; what was read before is kept. After
/// [`CompareLimits::attempts`] failures in a row on one read, or
/// [`CompareLimits::reconnects`] new connections in all, the comparison gives up and returns
/// what it has, with the rest listed as unread.
///
/// One kind of trouble is met without a new connection, see
/// [`CompareLimits::lost_answer_after`]: an answer lost together with the acknowledgement of
/// its request is acknowledged unseen and asked for again. Only a comparison does this; every
/// request it makes is a read (or the access key, which can be presented twice).
pub async fn compare<L: Link>(
    session: &mut Session<L>,
    download: Arc<Download>,
    options: PlanOptions,
    limits: CompareLimits,
    report: Reporter<'_>,
    cancel: &mut Cancel,
) -> Result<Comparison, Failure> {
    session.set_stale_after(limits.stale_after);
    session.set_lost_answer_after(limits.lost_answer_after);
    let mut tally = Tally::default();
    let mut reconnects = 0;

    // The pre-check, again from the start whenever the connection fails under it.
    let mut failures = 0;
    let found = loop {
        let prepared = Arc::clone(&download);
        let precheck = options.clone();
        let (outcome, trouble) = drive(session, report, cancel, &mut tally, None, move |device| {
            prepared.inspect(device, &precheck)
        })
        .await
        .map_err(|message| Failure::plain(message, tally.requests))?;
        let failure = match outcome {
            Ok(found) => break found,
            Err(failure) => failure,
        };
        failures += 1;
        let retry = worth_another_connection(&failure.error)
            && !matches!(trouble, Some(Trouble::Cancelled | Trouble::Link(_)))
            && failures < limits.attempts
            && reconnects < limits.reconnects;
        if !retry {
            return Err(Failure {
                requests: tally.requests,
                progress: None,
                ..Failure::from(failure)
            });
        }
        reconnects += 1;
        report(Report::Note(format!(
            "{}; starting again on a new connection",
            failure.error
        )));
        report(Report::Reconnecting);
        session.close().await;
        tokio::time::sleep(limits.reconnect_pause).await;
    };

    // A lost answer is handled here, by reconnecting, not by asking again on the same
    // connection.
    session.set_response_retries(0);
    let not_loaded = found
        .load_states
        .as_ref()
        .and_then(knxide_program::plan::LoadStates::not_loaded);
    if not_loaded.is_none() {
        report(Report::Phase("Reading the device"));
    }
    let variant = download.variant(found.kind);
    // What to read: every stretch the download sends, with the bytes planned there. Reads
    // without planned bytes are added further down.
    let mut chunks: Vec<(u16, usize, Option<Vec<u8>>)> = planned_reads(variant)
        .into_iter()
        .map(|(address, planned)| (address, planned.len(), Some(planned)))
        .collect();
    // What the comparison expects to send in all: what it has sent and the reads to come.
    // Told whenever it changes, so that whoever shows progress has a total that holds.
    let mut expected = 0;
    let mut expect = |sent: usize, to_come: usize, report: Reporter<'_>| {
        if sent + to_come != expected {
            expected = sent + to_come;
            report(Report::Expected(expected));
        }
    };
    expect(tally.requests, chunks.len(), report);
    let mut comparison = Comparison {
        found,
        planned: chunks.iter().map(|(_, length, _)| length).sum(),
        compared: 0,
        matching: 0,
        differing: 0,
        differences: Vec::new(),
        unread: Vec::new(),
        beside: Vec::new(),
        stopped_because: None,
        refused: false,
        not_loaded,
        repetitions: 0,
        re_requests: 0,
        reconnects: 0,
        requests: 0,
    };

    if let Some(what) = &comparison.not_loaded {
        // A device without its tables and application answers reads of that memory with no
        // data. The checking phase has said what there is to say.
        report(Report::Note(format!("{what}; its memory is not compared")));
        chunks.clear();
    }
    let mut reconnect_first = false;
    let mut done = 0;
    let mut rounds = 0;
    'chunks: loop {
        if comparison.not_loaded.is_some() {
            break;
        }
        if done == chunks.len() {
            // Everything the download sends is read. A device with another number of group
            // addresses than the plan has parts of its tables where the download sends
            // nothing; those are read too, so that the difference can be told in group
            // addresses. One table says where the next is, hence the rounds.
            rounds += 1;
            let seen: Vec<compare::Difference> = comparison
                .differences
                .iter()
                .map(|difference| compare::Difference {
                    address: difference.address,
                    device: difference.device.clone(),
                })
                .collect();
            let gaps = compare::table_gaps(variant, &seen, &comparison.beside);
            if gaps.is_empty() || rounds > 3 {
                break;
            }
            for (start, length) in gaps {
                for offset in (0..length).step_by(COMPARE_CHUNK) {
                    chunks.push((
                        start.wrapping_add(offset as u16),
                        (length - offset).min(COMPARE_CHUNK),
                        None,
                    ));
                }
            }
        }
        expect(tally.requests, chunks.len() - done, report);
        let Some((address, length, planned)) = chunks.get(done).cloned() else {
            break;
        };
        // Giving up on a read beside the plan does not make the comparison incomplete.
        let beside = planned.is_none();
        let read = Request::MemoryRead {
            address,
            count: length as u8,
        };
        let mut failures = 0;
        let device = loop {
            let attempt = async {
                if reconnect_first {
                    reconnects += 1;
                    report(Report::Reconnecting);
                    reconnect(
                        session,
                        variant.profile.mask_version,
                        options.key,
                        limits.reconnect_pause,
                        report,
                        cancel,
                        &mut tally,
                    )
                    .await?;
                }
                match exchange(session, &read, Expect::Answer, report, cancel, &mut tally).await? {
                    Some(Response::Memory {
                        address: answered,
                        data,
                    }) if answered == address && data.len() == length => Ok(data),
                    // An answer without data is the device declining to give this memory.
                    // That is its answer, not trouble on the way: asking again, on this
                    // connection or a new one, gets the same.
                    Some(Response::Memory {
                        address: answered,
                        data,
                    }) if answered == address && data.is_empty() => Err(Trouble::Refused),
                    Some(other) => Err(Trouble::Device(format!("{other} does not answer {read}"))),
                    None => Err(Trouble::Device(format!("no answer to {read}"))),
                }
            }
            .await;
            let trouble = match attempt {
                Ok(data) => {
                    reconnect_first = false;
                    break data;
                }
                Err(Trouble::Cancelled) => {
                    return Err(Failure {
                        message: "cancelled".into(),
                        check: None,
                        cancelled: true,
                        requests: tally.requests,
                        progress: None,
                    });
                }
                Err(Trouble::Link(what)) => {
                    if !beside {
                        comparison.stopped_because = Some(what);
                    }
                    break 'chunks;
                }
                Err(Trouble::Refused) => {
                    if !beside {
                        comparison.refused = true;
                        comparison.stopped_because = Some(format!(
                            "the device does not give its memory at {address:04X}: it answers \
                             the read with no data, as a device does whose application is not \
                             loaded or whose memory is protected"
                        ));
                    }
                    break 'chunks;
                }
                Err(Trouble::Device(what) | Trouble::AnswerLost(what)) => what,
            };
            failures += 1;
            if failures >= limits.attempts {
                if !beside {
                    comparison.stopped_because = Some(format!(
                        "{read} failed {failures} times in a row, the last time with: {trouble}"
                    ));
                }
                break 'chunks;
            }
            if reconnects >= limits.reconnects {
                if !beside {
                    comparison.stopped_because = Some(format!(
                        "{reconnects} new connections were not enough; the last trouble was: \
                         {trouble}"
                    ));
                }
                break 'chunks;
            }
            report(Report::Note(format!(
                "{trouble}; reconnecting and reading again"
            )));
            reconnect_first = true;
        };
        done += 1;
        let Some(planned) = planned else {
            comparison
                .beside
                .push(compare::Difference { address, device });
            continue;
        };
        comparison.compared += planned.len();
        for (offset, (planned, device)) in planned.iter().zip(&device).enumerate() {
            if planned == device {
                comparison.matching += 1;
                continue;
            }
            comparison.differing += 1;
            let at = address.wrapping_add(offset as u16);
            match comparison.differences.last_mut() {
                Some(last)
                    if u32::from(last.address) + last.planned.len() as u32 == u32::from(at) =>
                {
                    last.planned.push(*planned);
                    last.device.push(*device);
                }
                _ => comparison.differences.push(Difference {
                    address: at,
                    planned: vec![*planned],
                    device: vec![*device],
                }),
            }
        }
    }
    for (address, length, planned) in chunks.iter().skip(done) {
        if planned.is_none() {
            continue;
        }
        match comparison.unread.last_mut() {
            Some(last) if u32::from(last.address) + last.length as u32 == u32::from(*address) => {
                last.length += length;
            }
            _ => comparison.unread.push(Stretch {
                address: *address,
                length: *length,
            }),
        }
    }
    // Whatever was planned, this is what it came to.
    expect(tally.requests, 0, report);
    comparison.requests = tally.requests;
    comparison.repetitions = tally.repetitions;
    comparison.re_requests = tally.re_requests;
    comparison.reconnects = reconnects;
    Ok(comparison)
}

/// The reads a comparison plans for `download`: every stretch the download sends, in pieces
/// of [`COMPARE_CHUNK`] bytes, each with the bytes planned there.
pub fn planned_reads(download: &Download) -> Vec<(u16, Vec<u8>)> {
    let mut reads = Vec::new();
    for run in download.image.runs(0, 0x1_0000) {
        let Ok(start) = u16::try_from(run.address) else {
            continue;
        };
        for (index, planned) in run.data.chunks(COMPARE_CHUNK).enumerate() {
            let address = start.wrapping_add((index * COMPARE_CHUNK) as u16);
            reads.push((address, planned.to_vec()));
        }
    }
    reads
}

/// Close the connection and open a new one as at the start: a pause, connect, device
/// descriptor, access key.
async fn reconnect<L: Link>(
    session: &mut Session<L>,
    mask_version: u16,
    key: u32,
    pause: Duration,
    report: Reporter<'_>,
    cancel: &mut Cancel,
    tally: &mut Tally,
) -> Result<(), Trouble> {
    if session.is_open() {
        tally.requests += 1;
        report(Report::Request(Request::Disconnect));
    }
    session.close().await;
    tokio::time::sleep(pause).await;
    exchange(
        session,
        &Request::Connect,
        Expect::Nothing,
        report,
        cancel,
        tally,
    )
    .await?;
    let expected = mask_version;
    for request in [Request::DeviceDescriptorRead, Request::Authorize { key }] {
        match exchange(session, &request, Expect::Answer, report, cancel, tally).await? {
            Some(Response::DeviceDescriptor { mask_version }) if mask_version == expected => {}
            Some(Response::Authorize { level }) if level <= 2 => {}
            Some(other) => {
                return Err(Trouble::Device(format!(
                    "after reconnecting, {other} does not fit {request}"
                )));
            }
            None => {
                return Err(Trouble::Device(format!(
                    "after reconnecting, no answer to {request}"
                )));
            }
        }
    }
    Ok(())
}
