//! Bus and project health: problems found in the traffic the server sees.
//!
//! Issues accumulate from server start. Most clear themselves when the condition ends (a
//! stuck value changes, a quiet device speaks again); the rest stay until dismissed or the
//! project is reloaded.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use knxide_core::dpt::{DptError, DptId};
use knxide_core::{GroupAddress, IndividualAddress, LData, Payload};
use knxide_project::GroupAddressTable;
use serde::Serialize;

/// Thresholds.
#[derive(Debug, Clone, Copy)]
pub struct HealthConfig {
    /// A measured value (DPT 9 or 14) repeated unchanged for this long is reported as stuck.
    pub stuck_after: Duration,
    /// A device is quiet after this long, or ten of its usual intervals if that is longer.
    pub quiet_after: Duration,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            stuck_after: Duration::from_secs(6 * 3600),
            quiet_after: Duration::from_secs(3600),
        }
    }
}

/// Repeated frames from one device above this share are reported.
const REPEAT_SHARE: f64 = 0.02;
const REPEAT_MIN: u64 = 5;
/// A stuck value needs at least this many identical telegrams.
const STUCK_MIN_REPEATS: u64 = 3;
/// A device needs this many frames before its silence means anything.
const QUIET_MIN_FRAMES: u64 = 5;

/// What an issue is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// Something is going wrong on the bus or with a device.
    Bus,
    /// The ETS project does not describe the bus correctly.
    Project,
}

impl Category {
    fn of(kind: &str) -> Self {
        match kind {
            "dpt_mismatch" | "unknown_group" => Self::Project,
            _ => Self::Bus,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Info,
}

/// One problem, as shown on the page.
#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    /// Stable identity, e.g. `stuck/4/0/1`. Used to dismiss.
    pub key: String,
    pub kind: &'static str,
    pub severity: Severity,
    /// The group or physical address concerned, if any.
    pub subject: Option<String>,
    /// The device involved, for "go to device".
    pub device: Option<String>,
    /// Bus health, or the ETS project.
    pub category: Category,
    /// A DPT that would decode this address correctly, offered as a one-click override.
    pub suggested_dpt: Option<String>,
    pub title: String,
    pub detail: String,
    pub count: u64,
    pub first_seen: u64,
    pub last_seen: u64,
}

/// A problem to report; see [`Health::raise`].
struct Finding {
    key: String,
    kind: &'static str,
    severity: Severity,
    subject: Option<String>,
    device: Option<String>,
    title: String,
    detail: String,
}

struct ValueTrack {
    payload: Payload,
    since: u64,
    repeats: u64,
    source: IndividualAddress,
}

#[derive(Default)]
struct DeviceTrack {
    frames: u64,
    repeated: u64,
    first: u64,
    last: u64,
}

pub struct Health {
    config: HealthConfig,
    issues: BTreeMap<String, Issue>,
    values: HashMap<GroupAddress, ValueTrack>,
    devices: HashMap<IndividualAddress, DeviceTrack>,
    /// Set when the issue list changed and has not been broadcast yet.
    pub dirty: bool,
}

/// A problem with one telegram's value, shown next to it in the monitor.
pub fn value_warning(dpt: Option<DptId>, payload: &Payload) -> Option<String> {
    value_problem(dpt?, payload).map(|p| p.message)
}

/// What is wrong with a value under its DPT.
pub struct ValueProblem {
    pub message: String,
    /// The sensor sent its invalid-data marker: a device problem, not a typing one.
    pub invalid: bool,
    /// The value decodes, but a different DPT fits better.
    pub suggested: Option<DptId>,
}

pub fn value_problem(dpt: DptId, payload: &Payload) -> Option<ValueProblem> {
    let problem = |message: String| ValueProblem {
        message,
        invalid: false,
        suggested: None,
    };
    match dpt.decode(payload) {
        Ok(_) => float_in_disguise(dpt, payload).map(|v| ValueProblem {
            message: format!("looks like a float (DPT 14): {v}"),
            invalid: false,
            suggested: Some(DptId::main(14)),
        }),
        Err(DptError::InvalidData) => Some(ValueProblem {
            message: "the device reports this value as invalid".into(),
            invalid: true,
            suggested: None,
        }),
        Err(DptError::PayloadLength) => {
            Some(problem(format!("{} does not fit DPT {dpt}", size(payload))))
        }
        Err(err) => Some(problem(format!("not a valid {dpt} value: {err}"))),
    }
}

fn size(payload: &Payload) -> String {
    match payload {
        Payload::Small(_) => "a 6-bit value".into(),
        Payload::Bytes(b) if b.len() == 1 => "1 byte".into(),
        Payload::Bytes(b) => format!("{} bytes", b.len()),
    }
}

/// A 4-byte integer DPT whose integer reading is absurd while the float reading looks like a
/// measurement: probably a DPT 14 value under the wrong type. A guess, labelled as one.
fn float_in_disguise(dpt: DptId, payload: &Payload) -> Option<String> {
    if !matches!(dpt.main, 12 | 13) {
        return None;
    }
    let Payload::Bytes(bytes) = payload else {
        return None;
    };
    let raw: [u8; 4] = bytes.as_slice().try_into().ok()?;
    let as_int = i64::from(i32::from_be_bytes(raw)).abs();
    let as_float = f32::from_be_bytes(raw);
    let plausible = as_float.is_finite() && (0.01..1e8).contains(&as_float.abs());
    (as_int > 1_000_000_000 && plausible).then(|| format!("{as_float:.2}"))
}

impl Health {
    pub fn new(config: HealthConfig) -> Self {
        Self {
            config,
            issues: BTreeMap::new(),
            values: HashMap::new(),
            devices: HashMap::new(),
            dirty: false,
        }
    }

    pub fn issues(&self) -> Vec<Issue> {
        self.issues.values().cloned().collect()
    }

    pub fn dismiss(&mut self, key: &str) {
        if self.issues.remove(key).is_some() {
            self.dirty = true;
        }
    }

    /// The DPT of one address changed: its type findings no longer apply.
    pub fn address_changed(&mut self, ga: GroupAddress) {
        self.clear(&format!("dpt/{ga}"));
        self.clear(&format!("unknown/{ga}"));
    }

    /// The project changed: type and membership findings no longer apply.
    pub fn project_changed(&mut self) {
        let before = self.issues.len();
        self.issues
            .retain(|_, i| !matches!(i.kind, "dpt_mismatch" | "unknown_group"));
        self.dirty |= self.issues.len() != before;
    }

    fn raise(&mut self, f: Finding, now: u64) {
        let issue = self.issues.entry(f.key.clone()).or_insert_with(|| Issue {
            key: f.key,
            kind: f.kind,
            severity: f.severity,
            subject: f.subject,
            device: None,
            category: Category::of(f.kind),
            suggested_dpt: None,
            title: String::new(),
            detail: String::new(),
            count: 0,
            first_seen: now,
            last_seen: now,
        });
        issue.count += 1;
        issue.last_seen = now;
        issue.title = f.title;
        issue.detail = f.detail;
        issue.device = f.device;
        self.dirty = true;
    }

    fn clear(&mut self, key: &str) {
        if self.issues.remove(key).is_some() {
            self.dirty = true;
        }
    }

    /// Look at one frame from the bus. `own` is our tunnel address.
    pub fn observe(
        &mut self,
        ldata: &LData,
        project: &GroupAddressTable,
        own: Option<IndividualAddress>,
        now: u64,
    ) {
        if Some(ldata.source) == own {
            return;
        }
        // Devices and repeated frames. In an L_Data.ind, a clear repeat bit (Ctrl1 bit 5)
        // marks a repetition: the sender did not get an acknowledgement for the first try.
        // Spec 3/6/3 §4.1.5.3.3 and 3/2/2 §2.2.2.
        let device = self
            .devices
            .entry(ldata.source)
            .or_insert_with(|| DeviceTrack {
                first: now,
                ..Default::default()
            });
        device.frames += 1;
        device.last = now;
        if ldata.repeat_on_error {
            device.repeated += 1;
        }
        let (frames, repeated) = (device.frames, device.repeated);
        self.clear(&format!("quiet/{}", ldata.source));
        if repeated >= REPEAT_MIN && repeated as f64 / frames as f64 >= REPEAT_SHARE {
            let share = 100.0 * repeated as f64 / frames as f64;
            self.raise(
            Finding { device: Some(ldata.source.to_string()), key: format!("repeats/{}", ldata.source), kind: "repeated_frames", severity: Severity::Warning, subject: Some(ldata.source.to_string()), title: format!("{} repeats frames", ldata.source), detail: format!("{repeated} of {frames} frames ({share:.0} %) were repetitions: acknowledgements went missing. Check wiring, bus load or a failing receiver.") },
            now,
        );
        }

        let Some(ga) = ldata.group_address() else {
            return;
        };
        let payload = match ldata.apdu() {
            Some(
                knxide_core::Apdu::GroupValueWrite(p) | knxide_core::Apdu::GroupValueResponse(p),
            ) => Some(p),
            _ => None,
        };

        // Not in the project.
        if !project.is_empty() && project.get(ga).is_none() {
            self.raise(
            Finding { device: Some(ldata.source.to_string()), key: format!("unknown/{ga}"), kind: "unknown_group", severity: Severity::Info, subject: Some(ga.to_string()), title: format!("{ga} is not in the project"), detail: format!(
                    "Last from {}. Add it in ETS and re-export, or find out which device uses it.",
                    ldata.source
                ) },
            now,
        );
        }

        let Some(payload) = payload else { return };
        let entry = project.get(ga);
        let dpt = entry.and_then(|e| e.dpt).filter(|d| d.is_supported());
        let name = entry.map(|e| e.name.as_str()).filter(|n| !n.is_empty());
        let label = name.map_or_else(|| ga.to_string(), |n| format!("{ga} {n}"));

        // Value does not fit the DPT. A value that decodes but looks like another type is a
        // project note; one that cannot be decoded at all is a warning.
        if let Some(dpt) = dpt
            && let Some(problem) = value_problem(dpt, payload)
        {
            let (invalid, warning, suggested) =
                (problem.invalid, problem.message, problem.suggested);
            let key = format!("{}/{ga}", if invalid { "invalid" } else { "dpt" });
            self.raise(
                Finding {
                    key: key.clone(),
                    kind: if invalid {
                        "invalid_value"
                    } else {
                        "dpt_mismatch"
                    },
                    severity: if suggested.is_some() {
                        Severity::Info
                    } else {
                        Severity::Warning
                    },
                    subject: Some(ga.to_string()),
                    device: Some(ldata.source.to_string()),
                    title: if invalid {
                        format!("{label}: sensor reports invalid")
                    } else {
                        format!("{label}: value does not match DPT {dpt}")
                    },
                    detail: format!("{warning}. From {}, raw {payload}.", ldata.source),
                },
                now,
            );
            if let Some(issue) = self.issues.get_mut(&key) {
                issue.suggested_dpt = suggested.map(|d| d.to_string());
            }
        }

        // Stuck measured values: same value, again and again, for a long time.
        if dpt.is_some_and(|d| matches!(d.main, 9 | 14)) {
            let track = self.values.entry(ga).or_insert_with(|| ValueTrack {
                payload: payload.clone(),
                since: now,
                repeats: 0,
                source: ldata.source,
            });
            let changed = track.payload != *payload;
            if changed {
                *track = ValueTrack {
                    payload: payload.clone(),
                    since: now,
                    repeats: 1,
                    source: ldata.source,
                };
            } else {
                track.repeats += 1;
                track.source = ldata.source;
            }
            if changed {
                self.clear(&format!("stuck/{ga}"));
            }
        }
    }

    /// Time-based checks: stuck values and quiet devices.
    pub fn tick(&mut self, project: &GroupAddressTable, now: u64) {
        let stuck_ms = self.config.stuck_after.as_millis() as u64;
        let mut found = Vec::new();
        for (ga, track) in &self.values {
            let age = now.saturating_sub(track.since);
            if track.repeats >= STUCK_MIN_REPEATS && age >= stuck_ms {
                found.push((*ga, track.repeats, age, track.source, track.payload.clone()));
            }
        }
        for (ga, repeats, age, source, payload) in found {
            let entry = project.get(ga);
            let dpt = entry.and_then(|e| e.dpt);
            let value = dpt
                .and_then(|d| d.decode(&payload).ok().map(|v| d.format(&v).to_string()))
                .unwrap_or_else(|| payload.to_string());
            let name = entry
                .map(|e| e.name.clone())
                .filter(|n| !n.is_empty())
                .map_or(String::new(), |n| format!(" {n}"));
            let key = format!("stuck/{ga}");
            let previous = self.issues.get(&key);
            let first = previous.map_or(now, |i| i.first_seen);
            let old_text = previous.map(|i| (i.title.clone(), i.detail.clone()));
            let issue = Issue {
                key: key.clone(),
                kind: "stuck_value",
                severity: Severity::Warning,
                subject: Some(ga.to_string()),
                device: Some(source.to_string()),
                category: Category::Bus,
                suggested_dpt: None,
                title: format!("{ga}{name}: value stuck at {value}"),
                detail: format!(
                    "{source} has sent the same value {repeats} times over {}. The sensor may have hung; restarting {source} may help.",
                    human(age)
                ),
                count: repeats,
                first_seen: first,
                last_seen: now,
            };
            // Re-checked every tick; only a visible change needs broadcasting.
            if old_text != Some((issue.title.clone(), issue.detail.clone())) {
                self.dirty = true;
            }
            self.issues.insert(key, issue);
        }

        let quiet_min = self.config.quiet_after.as_millis() as u64;
        let mut quiet = Vec::new();
        for (ia, d) in &self.devices {
            if d.frames < QUIET_MIN_FRAMES {
                continue;
            }
            let interval = (d.last - d.first) / (d.frames - 1);
            let limit = quiet_min.max(interval.saturating_mul(10));
            let silent = now.saturating_sub(d.last);
            if silent > limit && !self.issues.contains_key(&format!("quiet/{ia}")) {
                quiet.push((*ia, silent, interval, d.frames));
            }
        }
        for (ia, silent, interval, frames) in quiet {
            self.raise(
                Finding {
                    device: Some(ia.to_string()),
                    key: format!("quiet/{ia}"),
                    kind: "quiet_device",
                    severity: Severity::Warning,
                    subject: Some(ia.to_string()),
                    title: format!("{ia} went quiet"),
                    detail: format!(
                        "Silent for {} after {frames} frames, normally every {}.",
                        human(silent),
                        human(interval)
                    ),
                },
                now,
            );
        }
    }

    /// The connection dropped.
    pub fn disconnected(&mut self, reason: &str, now: u64) {
        let reconnects = self.issues.get("connection").map_or(0, |i| i.count) + 1;
        self.raise(
            Finding {
                key: "connection".into(),
                kind: "connection",
                severity: Severity::Info,
                device: None,
                subject: None,
                title: format!(
                    "Connection dropped {reconnects} time{}",
                    if reconnects == 1 { "" } else { "s" }
                ),
                detail: format!("Last: {reason}."),
            },
            now,
        );
    }

    /// One of our writes was not confirmed on the bus.
    pub fn send_failed(&mut self, ga: GroupAddress, error: &str, now: u64) {
        self.raise(
            Finding {
                device: None,
                key: format!("send/{ga}"),
                kind: "send_failed",
                severity: Severity::Warning,
                subject: Some(ga.to_string()),
                title: format!("Writes to {ga} fail"),
                detail: format!("{error}."),
            },
            now,
        );
    }
}

fn human(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        3600..86400 => format!("{:.1} h", s as f64 / 3600.0),
        _ => format!("{:.1} days", s as f64 / 86400.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knxide_core::{Apdu, ga};

    const PROJECT: &str = r#"<GroupAddress-Export>
      <GroupAddress Name="Water temperature" Address="4/0/1" DPTs="DPST-9-1" />
      <GroupAddress Name="Meter kWh" Address="5/0/1" DPTs="DPST-13-10" />
      <GroupAddress Name="Light" Address="1/1/1" DPTs="DPST-1-1" />
    </GroupAddress-Export>"#;

    fn frame(source: &str, ga: GroupAddress, payload: Payload) -> LData {
        let mut l = LData::group(ga, Apdu::GroupValueWrite(payload));
        l.source = source.parse().unwrap();
        l
    }

    fn kinds(h: &Health) -> Vec<&'static str> {
        h.issues.values().map(|i| i.kind).collect()
    }

    #[test]
    fn mismatches_unknown_and_float_guess() {
        let project = GroupAddressTable::parse(PROJECT).unwrap();
        let mut h = Health::new(HealthConfig::default());
        h.observe(
            &frame("1.1.5", ga!(1 / 1 / 1), Payload::Bytes(vec![1, 2])),
            &project,
            None,
            0,
        );
        h.observe(
            &frame("1.1.5", ga!(5 / 0 / 9), Payload::Small(1)),
            &project,
            None,
            0,
        );
        // 321.25 as a float, sent to a 13.010 address.
        h.observe(
            &frame(
                "1.1.20",
                ga!(5 / 0 / 1),
                Payload::Bytes(vec![0x43, 0xa0, 0xa0, 0x00]),
            ),
            &project,
            None,
            0,
        );
        h.observe(
            &frame("1.1.40", ga!(4 / 0 / 1), Payload::Bytes(vec![0x7f, 0xff])),
            &project,
            None,
            0,
        );
        let issues = h.issues();
        let find = |key: &str| {
            issues
                .iter()
                .find(|i| i.key == key)
                .unwrap_or_else(|| panic!("{key}: {issues:#?}"))
        };
        assert!(
            find("dpt/1/1/1")
                .detail
                .contains("2 bytes does not fit DPT 1.001")
        );
        assert_eq!(find("unknown/5/0/9").severity, Severity::Info);
        let float = find("dpt/5/0/1");
        assert!(float.detail.contains("looks like a float (DPT 14): 321.25"));
        assert_eq!(
            (float.severity, float.category),
            (Severity::Info, Category::Project)
        );
        assert_eq!(float.suggested_dpt.as_deref(), Some("14"));
        assert_eq!(find("dpt/1/1/1").severity, Severity::Warning);
        assert_eq!(find("invalid/4/0/1").category, Category::Bus);
        assert_eq!(find("invalid/4/0/1").kind, "invalid_value");
        h.project_changed();
        assert_eq!(kinds(&h), ["invalid_value"]);
    }

    #[test]
    fn stuck_value_raised_and_cleared() {
        let project = GroupAddressTable::parse(PROJECT).unwrap();
        let mut h = Health::new(HealthConfig {
            stuck_after: Duration::from_secs(60),
            ..Default::default()
        });
        let water = |p: Vec<u8>| frame("1.1.40", ga!(4 / 0 / 1), Payload::Bytes(p));
        for t in [0, 30_000, 60_000] {
            h.observe(&water(vec![0x02, 0xb6]), &project, None, t);
        }
        h.tick(&project, 59_000);
        assert!(h.issues().is_empty());
        h.tick(&project, 61_000);
        let stuck = h.issues();
        assert_eq!(
            stuck[0].title,
            "4/0/1 Water temperature: value stuck at 6.94 °C"
        );
        assert!(stuck[0].detail.contains("restarting 1.1.40"));
        h.observe(&water(vec![0x02, 0xb7]), &project, None, 62_000);
        assert!(h.issues().is_empty(), "a change clears it");
    }

    #[test]
    fn quiet_devices_and_repeats() {
        let project = GroupAddressTable::parse(PROJECT).unwrap();
        let mut h = Health::new(HealthConfig {
            quiet_after: Duration::from_secs(10),
            ..Default::default()
        });
        for i in 0..6 {
            let mut f = frame("1.1.7", ga!(1 / 1 / 1), Payload::Small(1));
            f.repeat_on_error = true; // marked as a repetition
            h.observe(&f, &project, None, i * 1000);
        }
        assert_eq!(kinds(&h), ["repeated_frames"]);
        // Last frame at 5 s, threshold 10 s.
        h.tick(&project, 14_000);
        assert_eq!(kinds(&h), ["repeated_frames"]);
        h.tick(&project, 16_000);
        assert_eq!(kinds(&h), ["quiet_device", "repeated_frames"]);
        h.observe(
            &frame("1.1.7", ga!(1 / 1 / 1), Payload::Small(0)),
            &project,
            None,
            20_000,
        );
        assert_eq!(kinds(&h), ["repeated_frames"]);
        h.dismiss("repeats/1.1.7");
        assert!(h.issues().is_empty());
        // Our own frames are ignored.
        h.observe(
            &frame("1.1.252", ga!(7 / 7 / 7), Payload::Small(0)),
            &project,
            "1.1.252".parse().ok(),
            0,
        );
        assert!(h.issues().is_empty());
    }

    #[test]
    fn connection_and_send_failures() {
        let mut h = Health::new(HealthConfig::default());
        h.disconnected("heartbeat failed", 0);
        h.disconnected("server closed the connection", 1);
        h.send_failed(
            ga!(1 / 1 / 1),
            "interface could not send the frame on the bus",
            2,
        );
        let issues = h.issues();
        assert_eq!(issues[0].title, "Connection dropped 2 times");
        assert_eq!(issues[1].title, "Writes to 1/1/1 fail");
    }
}
