//! Replay recorded downloads.
//!
//! The recordings are private (manufacturer product data and a real installation), so they are
//! not in the repository. Point `KNXIDE_PROGRAM_FIXTURES` at a directory with one
//! sub-directory per recorded download; without it these tests print a note and pass.
//!
//! Each sub-directory holds `app.xml` (application program), `device.xml` (the project's
//! `DeviceInstance`), `group-addresses.txt` (`GA-<n> <address>` lines), `meta.txt`
//! (`key=value` lines, `mask` among them), `requests.txt` and `responses.txt` (one request or
//! response per line, in bus order) and `memory-final.txt` (`ADDR HEX` runs of memory the
//! download wrote or read and accepted).
//!
//! Run with `--nocapture` to see the per-recording figures.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use knxide_program::{
    ApplicationProgram, DeviceData, Download, DownloadKind, GroupAddresses, PlanOptions, Request,
    Response, SimulatedDevice,
};

const VARIABLE: &str = "KNXIDE_PROGRAM_FIXTURES";

struct Recording {
    name: String,
    program: ApplicationProgram,
    device: DeviceData,
    addresses: GroupAddresses,
    mask: u16,
    requests: Vec<Request>,
    responses: Vec<Response>,
    memory: BTreeMap<u32, u8>,
}

fn recordings() -> Vec<Recording> {
    let Some(root) = std::env::var_os(VARIABLE).map(PathBuf::from) else {
        eprintln!("skipped: {VARIABLE} is not set");
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        eprintln!("skipped: {VARIABLE} does not name a readable directory");
        return Vec::new();
    };
    let mut directories: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.join("app.xml").is_file())
        .collect();
    directories.sort();
    if directories.is_empty() {
        eprintln!("skipped: no recordings under {VARIABLE}");
    }
    directories.iter().map(|path| load(path)).collect()
}

fn load(directory: &Path) -> Recording {
    let read = |file: &str| {
        std::fs::read_to_string(directory.join(file))
            .unwrap_or_else(|error| panic!("{}: {error}", directory.join(file).display()))
    };
    let lines = |file: &str| -> Vec<String> {
        read(file)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mask = lines("meta.txt")
        .iter()
        .find_map(|line| line.strip_prefix("mask=").map(str::to_string))
        .and_then(|hex| u16::from_str_radix(&hex, 16).ok())
        .expect("mask in meta.txt");
    let mut memory = BTreeMap::new();
    for line in lines("memory-final.txt") {
        let (address, hex) = line.split_once(' ').expect("ADDR HEX");
        let address = u32::from_str_radix(address, 16).expect("address");
        for at in (0..hex.len()).step_by(2) {
            let byte = u8::from_str_radix(&hex[at..at + 2], 16).expect("hex");
            memory.insert(address + (at / 2) as u32, byte);
        }
    }
    Recording {
        name: directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        program: ApplicationProgram::parse(&read("app.xml")).expect("app.xml"),
        device: DeviceData::parse(&read("device.xml")).expect("device.xml"),
        addresses: GroupAddresses::parse_list(&read("group-addresses.txt"))
            .expect("group-addresses.txt"),
        mask,
        requests: lines("requests.txt")
            .iter()
            .map(|line| line.parse().expect("request line"))
            .collect(),
        responses: lines("responses.txt")
            .iter()
            .map(|line| line.parse().expect("response line"))
            .collect(),
        memory,
    }
}

impl Recording {
    /// What "download the application" prepares: full, or partial with the full download as
    /// fallback when the product prefers that.
    fn prepared(&self) -> Download {
        Download::prepare(&self.program, &self.device, &self.addresses).expect("prepare")
    }

    /// The kind of download the recorded device's answers lead to.
    fn kind(&self) -> DownloadKind {
        self.prepared()
            .precheck(&mut self.device(), &PlanOptions::default())
            .expect("pre-check against the recording")
            .kind
    }

    /// The kind the recording is not; used as a control.
    fn other_kind(&self) -> DownloadKind {
        if self.kind().full {
            DownloadKind::PARTIAL
        } else {
            DownloadKind::FULL
        }
    }

    fn download(&self, kind: DownloadKind) -> Download {
        Download::build(&self.program, &self.device, &self.addresses, kind).expect("build")
    }

    fn device(&self) -> SimulatedDevice {
        SimulatedDevice::from_recording(self.mask, &self.requests, &self.responses)
            .expect("requests and responses pair up")
    }
}

/// Milestone A: every recorded byte that lies in a product segment equals the image, and the
/// image sends exactly the recorded bytes of those segments.
#[test]
fn images_match_recorded_memory() {
    for recording in recordings() {
        let download = recording.download(recording.kind());
        let image = &download.image;
        let mut report = String::new();
        let (mut compared, mut matching) = (0, 0);
        for (address, recorded) in &recording.memory {
            let Some(built) = image.byte(*address) else {
                continue;
            };
            compared += 1;
            if built == *recorded {
                matching += 1;
            } else if compared - matching <= 16 {
                let _ = writeln!(
                    report,
                    "  {address:04X}: recorded {recorded:02X}, built {built:02X}"
                );
            }
        }
        // Which bytes are sent: the recording against the image's own idea.
        let mut sent_not_recorded = Vec::new();
        let mut recorded_not_sent = Vec::new();
        for segment in image.segments() {
            for offset in 0..segment.data.len() as u32 {
                let address = segment.address + offset;
                match (
                    image.is_sent(address),
                    recording.memory.contains_key(&address),
                ) {
                    (true, false) => sent_not_recorded.push(address),
                    (false, true) => recorded_not_sent.push(address),
                    _ => {}
                }
            }
        }
        let tables = download.tables.as_ref();
        println!(
            "{}: kind {:?}; {} parameter references in use, {} group object references in use, \
             {} group addresses, {} associations; image bytes compared {compared}, matching \
             {matching}; sent but not recorded {}, recorded but not sent {}",
            recording.name,
            download.kind,
            download.configuration.active_parameter_count(),
            download.configuration.active_object_count(),
            tables.map_or(0, |tables| tables.addresses.len()),
            tables.map_or(0, |tables| tables.associations.len()),
            sent_not_recorded.len(),
            recorded_not_sent.len(),
        );
        // Control: the other kind of download sends a different set of bytes.
        let control = recording.download(recording.other_kind());
        let differing = image
            .segments()
            .iter()
            .flat_map(|segment| (0..segment.data.len() as u32).map(|at| segment.address + at))
            .filter(|address| image.is_sent(*address) != control.image.is_sent(*address))
            .count();
        println!(
            "{}: control, kind {:?}: {differing} bytes are sent in one kind and not the other",
            recording.name, control.kind
        );
        assert!(compared > 0, "{}: nothing to compare", recording.name);
        assert_eq!(compared, matching, "{}:\n{report}", recording.name);
        assert!(
            sent_not_recorded.is_empty() && recorded_not_sent.is_empty(),
            "{}: sent but not recorded {:04X?}, recorded but not sent {:04X?}",
            recording.name,
            &sent_not_recorded[..sent_not_recorded.len().min(16)],
            &recorded_not_sent[..recorded_not_sent.len().min(16)],
        );
    }
}

fn kind_of(request: &Request) -> &'static str {
    match request {
        Request::Connect => "T_CONNECT",
        Request::Disconnect => "T_DISCONNECT",
        Request::DeviceDescriptorRead => "DEVICE_DESCRIPTOR_READ",
        Request::Authorize { .. } => "AUTHORIZE_REQUEST",
        Request::PropertyRead { .. } => "PROP_READ",
        Request::PropertyWrite { .. } => "PROP_WRITE",
        Request::MemoryRead { .. } => "MEM_READ",
        Request::MemoryWrite { .. } => "MEM_WRITE",
        Request::Restart => "RESTART",
    }
}

fn counts(requests: &[Request]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for request in requests {
        *counts.entry(kind_of(request)).or_insert(0) += 1;
    }
    counts
}

/// The comparison of one planning run with the recording.
struct Comparison {
    planned: Vec<Request>,
    /// Number of leading lines that are equal.
    equal_prefix: usize,
    failure: Option<String>,
    modelled: usize,
}

fn plan(recording: &Recording, download: &Download) -> Comparison {
    let mut device = recording.device();
    let (planned, failure) = match download.plan(&mut device, &PlanOptions::default()) {
        Ok(plan) => (plan.requests, None),
        Err(failure) => (failure.requests.clone(), Some(failure.error.to_string())),
    };
    let equal_prefix = planned
        .iter()
        .zip(&recording.requests)
        .take_while(|(planned, recorded)| planned == recorded)
        .count();
    Comparison {
        planned,
        equal_prefix,
        failure,
        modelled: device.modelled(),
    }
}

fn describe(recording: &Recording, label: &str, comparison: &Comparison) -> String {
    let recorded = &recording.requests;
    let planned = &comparison.planned;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} [{label}]: recorded {}, planned {}, equal leading lines {}, answers taken from \
         the model {}",
        recording.name,
        recorded.len(),
        planned.len(),
        comparison.equal_prefix,
        comparison.modelled,
    );
    if let Some(failure) = &comparison.failure {
        let _ = writeln!(out, "  planning stopped: {failure}");
    }
    if comparison.equal_prefix < recorded.len().max(planned.len()) {
        let line = comparison.equal_prefix;
        let show = |list: &[Request]| {
            list.get(line)
                .map_or("(end)".to_string(), |r| r.to_string())
        };
        let _ = writeln!(
            out,
            "  first difference at line {}: recorded {}, planned {}",
            line + 1,
            show(recorded),
            show(planned)
        );
    }
    let (recorded_counts, planned_counts) = (counts(recorded), counts(planned));
    let mut kinds: Vec<&str> = recorded_counts
        .keys()
        .chain(planned_counts.keys())
        .copied()
        .collect();
    kinds.sort();
    kinds.dedup();
    for kind in kinds {
        let (r, p) = (
            recorded_counts.get(kind).copied().unwrap_or(0),
            planned_counts.get(kind).copied().unwrap_or(0),
        );
        let _ = writeln!(
            out,
            "  {kind:<24} recorded {r:>4} planned {p:>4}{}",
            if r == p { "" } else { "  <-- differs" }
        );
    }
    out
}

/// Milestone B: the planned requests against the recorded ones, line by line.
///
/// Every request is generated from rules: the opening, the pre-check, the choice between a
/// full and a partial download and the procedure. Nothing is taken from the recording but the
/// device's answers, and the plan must equal the recording exactly, from the first line to the
/// last.
#[test]
fn plans_match_recorded_requests() {
    for recording in recordings() {
        let prepared = recording.prepared();
        let by_the_rules = plan(&recording, &prepared);
        let first = describe(&recording, "as prepared", &by_the_rules);
        // Control: the kind of download the device's answers did not lead to must not fit.
        let other = recording.download(recording.other_kind());
        let control = plan(&recording, &other);
        let second = describe(&recording, "control: other kind of download", &control);
        println!(
            "{}: kind {:?}; requests taken from the recording: 0; hardware identification \
             compared with {:02X?}\n{first}{second}",
            recording.name,
            recording.kind(),
            prepared.hardware
        );
        assert!(
            control.equal_prefix < recording.requests.len(),
            "the other kind of download reproduces the recording too\n{second}"
        );
        assert_eq!(by_the_rules.failure, None, "planning stopped\n{first}");
        assert_eq!(
            by_the_rules.planned, recording.requests,
            "the plan differs from the recording\n{first}"
        );
        assert_eq!(
            by_the_rules.modelled, 0,
            "an answer had to be invented\n{first}"
        );
    }
}
