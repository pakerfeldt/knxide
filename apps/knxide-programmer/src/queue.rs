//! Programming several devices in one go: a queue of ordinary downloads, one after another.
//!
//! The queue adds nothing a single download would not do. Each device is downloaded as its
//! own job, with the same checks before it and the same recovery in it. A download that does
//! not go through stops the queue, and so does cancelling; the devices behind it are left as
//! they are, with their edits pending.

use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::Serialize;
use serde_json::{Value, json};

use super::{Bus, JobKind, JobStatus, Programmer, Refusal, body_json, guard, json_response, ok};
use crate::workspace::Open;

/// How one device of a queue stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EntryStatus {
    /// Its turn has not come.
    Waiting,
    /// Its download is running.
    Running,
    /// Its download went through.
    Done,
    /// Its download did not go through, or could not be started.
    Failed,
    /// The queue stopped before its turn; its edits are still pending.
    Pending,
}

/// One device of a queue.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Entry {
    device: String,
    name: String,
    status: EntryStatus,
    /// The download job, once it has started.
    job: Option<u64>,
    /// Why it failed, where no job says so.
    error: Option<String>,
}

/// The devices to program, in order, and how far it got.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Queue {
    id: u64,
    entries: Vec<Entry>,
    /// Whether it is still at work.
    pub(crate) active: bool,
    /// Whether it was told to stop.
    #[serde(skip)]
    stopped: bool,
}

impl Programmer {
    /// Start programming the devices `ids`, in that order. Nothing is started unless every
    /// one of them can be programmed.
    fn program(&self, ids: &[String], confirmed: bool) -> Result<Value, Refusal> {
        let unprocessable =
            |message: String| Refusal::new(StatusCode::UNPROCESSABLE_ENTITY, message);
        if ids.is_empty() {
            return Err(Refusal::new(
                StatusCode::BAD_REQUEST,
                "name the devices to program",
            ));
        }
        let mut entries = Vec::new();
        for id in ids {
            let device = self.device(id)?;
            let name = if device.name.is_empty() {
                device
                    .address
                    .map(|address| address.to_string())
                    .unwrap_or_default()
            } else {
                device.name.clone()
            };
            if entries.iter().any(|entry: &Entry| entry.device == *id) {
                return Err(unprocessable(format!("{name} is named twice")));
            }
            if !device.is_supported() {
                let reason = crate::view::reason(device).unwrap_or_default();
                return Err(unprocessable(format!(
                    "{name} cannot be programmed here: {reason}"
                )));
            }
            if let Some(Err(error)) = self
                .effective(device)
                .map(|effective| effective.download.clone().map(drop))
            {
                return Err(unprocessable(format!(
                    "{name}: the download cannot be prepared: {error}"
                )));
            }
            entries.push(Entry {
                device: id.clone(),
                name,
                status: EntryStatus::Waiting,
                job: None,
                error: None,
            });
        }
        if matches!(self.inner.bus, Bus::None) {
            return Err(Refusal::new(StatusCode::CONFLICT, "no bus is configured"));
        }
        if !confirmed {
            return Err(Refusal::new(
                StatusCode::BAD_REQUEST,
                "programming has to be confirmed: send {\"confirm\": true}",
            ));
        }
        let queue = {
            let mut shared = self.shared();
            let busy = shared
                .job
                .as_ref()
                .is_some_and(|job| job.status == JobStatus::Running)
                || shared.queue.as_ref().is_some_and(|queue| queue.active);
            if busy {
                return Err(Refusal::new(
                    StatusCode::CONFLICT,
                    "another operation is running",
                ));
            }
            shared.next_queue += 1;
            let queue = Queue {
                id: shared.next_queue,
                entries,
                active: true,
                stopped: false,
            };
            shared.queue = Some(queue.clone());
            queue
        };
        self.broadcast(&json!({ "type": "queue", "queue": queue }));
        let programmer = self.clone();
        let id = queue.id;
        tokio::spawn(async move { programmer.work_through(id).await });
        Ok(json!({ "queue": queue }))
    }

    /// Change the queue `id` and tell the pages.
    fn queue_step(&self, id: u64, change: impl FnOnce(&mut Queue)) {
        let queue = {
            let mut shared = self.shared();
            let Some(queue) = shared.queue.as_mut().filter(|queue| queue.id == id) else {
                return;
            };
            change(queue);
            queue.clone()
        };
        self.broadcast(&json!({ "type": "queue", "queue": queue }));
    }

    /// Download the devices of queue `id` one after another, until one does not go through
    /// or the queue is told to stop.
    async fn work_through(&self, id: u64) {
        let devices: Vec<String> = match &self.shared().queue {
            Some(queue) if queue.id == id => queue
                .entries
                .iter()
                .map(|entry| entry.device.clone())
                .collect(),
            _ => return,
        };
        for (at, device) in devices.iter().enumerate() {
            let stopped = self
                .shared()
                .queue
                .as_ref()
                .is_none_or(|queue| queue.stopped);
            if stopped {
                break;
            }
            let job = match self.start(JobKind::Download, device, true, true) {
                Ok(job) => job,
                Err(refusal) => {
                    self.queue_step(id, |queue| {
                        queue.entries[at].status = EntryStatus::Failed;
                        queue.entries[at].error = Some(refusal.1);
                    });
                    break;
                }
            };
            self.queue_step(id, |queue| {
                queue.entries[at].status = EntryStatus::Running;
                queue.entries[at].job = Some(job);
            });
            let status = loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let shared = self.shared();
                match shared.job.as_ref().filter(|running| running.id == job) {
                    Some(running) if running.status == JobStatus::Running => {}
                    Some(finished) => break finished.status,
                    // Another job took its place: it is over.
                    None => break JobStatus::Failed,
                }
            };
            let done = status == JobStatus::Succeeded;
            self.queue_step(id, |queue| {
                queue.entries[at].status = if done {
                    EntryStatus::Done
                } else {
                    EntryStatus::Failed
                };
            });
            if !done {
                break;
            }
        }
        self.queue_step(id, |queue| {
            for entry in &mut queue.entries {
                if entry.status == EntryStatus::Waiting {
                    entry.status = EntryStatus::Pending;
                }
            }
            queue.active = false;
        });
    }

    /// Tell a queue at work to stop after the download that is running. The caller cancels
    /// that download.
    pub(crate) fn stop_queue(&self) {
        if let Some(queue) = self.shared().queue.as_mut().filter(|queue| queue.active) {
            queue.stopped = true;
        }
    }
}

pub(crate) async fn start(
    Open(programmer): Open,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let body = body_json(&body)?;
    let Some(ids) = body["devices"].as_array() else {
        return Err(Refusal::new(
            StatusCode::BAD_REQUEST,
            "name the devices to program",
        ));
    };
    let ids: Vec<String> = ids
        .iter()
        .map(|id| id.as_str().map(str::to_string))
        .collect::<Option<_>>()
        .ok_or_else(|| Refusal::new(StatusCode::BAD_REQUEST, "name the devices to program"))?;
    let started = programmer.program(&ids, body["confirm"] == true)?;
    Ok(json_response(StatusCode::ACCEPTED, &started))
}

pub(crate) async fn status(Open(programmer): Open) -> Response {
    let queue = programmer.shared().queue.clone();
    ok(&json!({ "queue": queue }))
}
