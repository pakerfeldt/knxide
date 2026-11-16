//! Downloads and comparisons end to end, through the transport layer, against simulated
//! devices: no socket is opened. The devices are those of the test kit's made-up project.
//!
//! With `KNXIDE_PROGRAM_FIXTURES` set, the recorded downloads are replayed the same way and
//! the requests the simulated device takes are compared with the recorded ones.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use knxide_core::IndividualAddress;
use knxide_core::Tpdu;
use knxide_program::transport::TransportConfig;
use knxide_program::{
    ApplicationProgram, DeviceData, Download, DownloadKind, GroupAddresses, PlanOptions, Project,
    ProjectArchive, Request, Response, SimulatedDevice,
};
use knxide_programmer::link::{Link, LinkError};
use knxide_programmer::ops::{self, Cancel, CompareLimits, DownloadLimits, Failure, Report};
use knxide_programmer::session::{Session, SessionConfig};
use knxide_programmer::sim::{Faults, SimBus, SimLink};

const TOOL: IndividualAddress = IndividualAddress::from_raw(0x0002);
const DEVICE: IndividualAddress = IndividualAddress::from_raw(0x1109);

/// Short timers, so that lost frames cost milliseconds.
fn fast() -> SessionConfig {
    SessionConfig {
        transport: TransportConfig {
            ack_timeout: Duration::from_millis(40),
            repetitions: 3,
            response_timeout: Duration::from_millis(200),
            lost_answer_timeout: None,
            unanswered_timeout: None,
        },
        response_retries: 1,
        stale_after: None,
    }
}

/// Short pauses to go with the short timers. Silence is not watched, and answers are not
/// taken as lost, unless a test asks.
fn limits() -> CompareLimits {
    CompareLimits {
        reconnect_pause: Duration::from_millis(5),
        stale_after: None,
        lost_answer_after: None,
        ..CompareLimits::default()
    }
}

/// The prepared downloads of the made-up project's three supported devices, by mask.
fn downloads() -> Vec<(u16, Arc<Download>)> {
    let bytes = knxide_testkit::project::archive();
    let project = Project::load(&ProjectArchive::open(&bytes).unwrap()).unwrap();
    project
        .devices
        .iter()
        .filter(|device| device.is_supported())
        .map(|device| {
            let download = device.download.clone().unwrap();
            (download.profile.mask_version, Arc::new(download))
        })
        .collect()
}

fn download_for(mask: u16) -> Arc<Download> {
    downloads()
        .into_iter()
        .find(|(candidate, _)| *candidate == mask)
        .unwrap()
        .1
}

fn strict() -> PlanOptions {
    PlanOptions {
        require_same_application: true,
        ..PlanOptions::default()
    }
}

struct Run {
    bus: SimBus,
    reports: Vec<Report>,
    limits: CompareLimits,
}

impl Run {
    /// A bus with `device` at [`DEVICE`].
    fn with(device: SimulatedDevice) -> Self {
        let bus = SimBus::new();
        bus.add(DEVICE, device);
        Self {
            bus,
            reports: Vec::new(),
            limits: limits(),
        }
    }

    fn session(&self) -> Session<SimLink> {
        Session::new(self.bus.link(TOOL), DEVICE, fast())
    }

    async fn download(
        &mut self,
        download: &Arc<Download>,
        options: PlanOptions,
    ) -> Result<knxide_program::Plan, Failure> {
        let mut session = self.session();
        let reports = &mut self.reports;
        let mut report = |report: Report| reports.push(report);
        // As a download without recovery goes: the tests of the recovery say so themselves.
        let result = ops::download(
            &mut session,
            Arc::clone(download),
            options,
            DownloadLimits::none(),
            &mut report,
            &mut Cancel::never(),
        )
        .await;
        session.finish().await;
        result.map(|downloaded| downloaded.plan)
    }

    async fn compare(&mut self, download: &Arc<Download>) -> Result<ops::Comparison, Failure> {
        let mut session = self.session();
        let reports = &mut self.reports;
        let mut report = |report: Report| reports.push(report);
        let result = ops::compare(
            &mut session,
            Arc::clone(download),
            strict(),
            self.limits,
            &mut report,
            &mut Cancel::never(),
        )
        .await;
        session.finish().await;
        result
    }

    fn received(&self) -> Vec<String> {
        self.bus
            .with(DEVICE, |endpoint| {
                endpoint
                    .received()
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap()
    }

    fn notes(&self) -> Vec<&str> {
        self.reports
            .iter()
            .filter_map(|report| match report {
                Report::Note(note) => Some(note.as_str()),
                _ => None,
            })
            .collect()
    }

    fn set_faults(&self, faults: Faults) {
        self.bus.with(DEVICE, |endpoint| endpoint.faults = faults);
    }
}

fn lines(requests: &[Request]) -> Vec<String> {
    requests.iter().map(ToString::to_string).collect()
}

fn is_write(line: &str) -> bool {
    line.starts_with("MEM_WRITE") || line.starts_with("PROP_WRITE") || line == "RESTART"
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_go_through_the_transport() {
    for (mask, download) in downloads() {
        // What the planner sends when it talks to the model directly ...
        let direct = download
            .plan(&mut SimulatedDevice::holding(&download), &strict())
            .unwrap();
        // ... is what the device takes when frames, sequence numbers and acknowledgements
        // are in between.
        let mut run = Run::with(SimulatedDevice::holding(&download));
        let plan = run.download(&download, strict()).await.unwrap();
        assert_eq!(plan.requests, direct.requests, "mask {mask:04X}");
        assert_eq!(run.received(), lines(&plan.requests), "mask {mask:04X}");
        assert!(run.notes().is_empty(), "{:?}", run.notes());
        // More than sixteen numbered frames each way: the sequence numbers have wrapped.
        assert!(plan.requests.len() > 20);

        // The device now holds every byte the download sends.
        let variant = download.variant(plan.kind);
        for segment_run in variant.image.runs(0, 0x1_0000) {
            for (offset, byte) in segment_run.data.iter().enumerate() {
                let address = (segment_run.address as usize + offset) as u16;
                let held = run
                    .bus
                    .with(DEVICE, |endpoint| endpoint.device().memory(address))
                    .unwrap();
                assert_eq!(held, *byte, "mask {mask:04X} address {address:04X}");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn comparing_reads_and_never_writes() {
    for (mask, download) in downloads() {
        let mut run = Run::with(SimulatedDevice::holding(&download));
        // An empty device differs wherever the image is not zero.
        let before = run.compare(&download).await.unwrap();
        assert!(before.differing > 0, "mask {mask:04X}");
        assert_eq!(before.reconnects, 0);
        let sent: usize = download
            .variant(before.found.kind)
            .image
            .runs(0, 0x1_0000)
            .iter()
            .map(|run| run.data.len())
            .sum();
        assert_eq!(before.compared, sent);
        assert_eq!(
            before.differing,
            before
                .differences
                .iter()
                .map(|difference| difference.planned.len())
                .sum::<usize>()
        );
        let received = run.received();
        assert!(!received.iter().any(|line| is_write(line)), "{received:?}");
        assert_eq!(received.len(), before.requests + 1, "plus the disconnect");
        assert_eq!(received.last().unwrap(), "T_DISCONNECT");
        // Twelve bytes at most per read.
        for line in &received {
            if let Ok(Request::MemoryRead { count, .. }) = line.parse() {
                assert!(count <= 12);
            }
        }

        // After a download nothing differs.
        run.download(&download, strict()).await.unwrap();
        let after = run.compare(&download).await.unwrap();
        assert_eq!(
            after.differing, 0,
            "mask {mask:04X}: {:?}",
            after.differences
        );
        assert_eq!(after.compared, before.compared);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_acknowledgement_is_answered_with_a_repetition() {
    let download = download_for(0x0705);
    let mut run = Run::with(SimulatedDevice::holding(&download));
    // The device takes the first three numbered frames and answers them, but its
    // acknowledgements are lost.
    run.set_faults(Faults {
        lose_acks: 3,
        ..Faults::default()
    });
    let plan = run.download(&download, strict()).await.unwrap();
    // Each request reached the device once, although three were sent twice.
    assert_eq!(run.received(), lines(&plan.requests));
    let repeated: Vec<&str> = run
        .notes()
        .into_iter()
        .filter(|note| note.contains("sent again"))
        .collect();
    assert_eq!(repeated.len(), 3, "{repeated:?}");
    assert!(repeated[0].starts_with("DEVICE_DESCRIPTOR_READ"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_request_is_repeated() {
    let download = download_for(0x0701);
    let mut run = Run::with(SimulatedDevice::holding(&download));
    run.set_faults(Faults {
        lose_requests: 2,
        ..Faults::default()
    });
    let plan = run.download(&download, strict()).await.unwrap();
    assert_eq!(run.received(), lines(&plan.requests));
    assert_eq!(
        run.notes()
            .iter()
            .filter(|note| note.contains("sent again"))
            .count(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_never_acknowledges_ends_the_download() {
    let download = download_for(0x0021);
    let mut run = Run::with(SimulatedDevice::holding(&download));
    run.set_faults(Faults {
        lose_requests: u32::MAX,
        ..Faults::default()
    });
    let failure = run.download(&download, strict()).await.unwrap_err();
    assert!(
        failure.message.contains("did not acknowledge"),
        "{}",
        failure.message
    );
    // Four transmissions of the first numbered frame, then nothing more.
    assert_eq!(
        run.notes()
            .iter()
            .filter(|note| note.contains("sent again"))
            .count(),
        3
    );
    assert_eq!(run.received(), ["T_CONNECT", "T_DISCONNECT"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_answer_while_comparing_reconnects_and_reads_again() {
    // BCU 2: the pre-check reads no memory, so the first memory read belongs to the comparison.
    let download = download_for(0x0021);
    let bus = SimBus::new();
    bus.add(DEVICE, SimulatedDevice::holding(&download));
    let mut session = Session::new(bus.link(TOOL), DEVICE, fast());
    let mut armed = false;
    let faulty = bus.clone();
    let mut report = move |report: Report| {
        if let Report::Request(Request::MemoryRead { .. }) = report
            && !armed
        {
            armed = true;
            faulty.with(DEVICE, |endpoint| endpoint.faults.lose_responses = 1);
        }
    };
    let comparison = ops::compare(
        &mut session,
        Arc::clone(&download),
        strict(),
        limits(),
        &mut report,
        &mut Cancel::never(),
    )
    .await
    .unwrap();
    session.finish().await;
    assert_eq!(comparison.reconnects, 1);
    assert!(comparison.is_complete());

    let received: Vec<String> = bus
        .with(DEVICE, |endpoint| {
            endpoint
                .received()
                .iter()
                .map(ToString::to_string)
                .collect()
        })
        .unwrap();
    let first_read = received
        .iter()
        .position(|line| line.starts_with("MEM_READ"))
        .unwrap();
    // A new connection as at the start, then the same read again.
    assert_eq!(
        received[first_read + 1..first_read + 6],
        [
            "T_DISCONNECT",
            "T_CONNECT",
            "DEVICE_DESCRIPTOR_READ",
            "AUTHORIZE_REQUEST FFFFFFFF",
            received[first_read].as_str(),
        ]
    );
    assert!(!received.iter().any(|line| is_write(line)));
    // The comparison is complete all the same.
    let sent: usize = download
        .image
        .runs(0, 0x1_0000)
        .iter()
        .map(|run| run.data.len())
        .sum();
    assert_eq!(comparison.compared, sent);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unexpected_load_state_aborts_the_download() {
    let download = download_for(0x0705);
    let mut device = SimulatedDevice::holding(&download);
    // The device answers the first load event of the application's state machine with
    // "error" (3) instead of "loading" (2).
    let load: Request = "PROP_WRITE obj=3 pid=5 start=1 count=1 01000000000000000000"
        .parse()
        .unwrap();
    let error: Response = "PROP_RESPONSE obj=3 pid=5 start=1 count=1 03"
        .parse()
        .unwrap();
    device
        .replay(std::slice::from_ref(&load), &[error])
        .unwrap();
    let mut run = Run::with(device);
    let failure = run.download(&download, strict()).await.unwrap_err();
    assert!(
        failure
            .message
            .contains("load state machine 3 is in state 3"),
        "{}",
        failure.message
    );
    assert_eq!(failure.check, None);
    // The failing event is the last thing sent before the connection is closed.
    let received = run.received();
    assert_eq!(received.len(), failure.requests + 1);
    assert_eq!(
        received[received.len() - 2..],
        [load.to_string().as_str(), "T_DISCONNECT"]
    );
    assert!(!received.contains(&"RESTART".to_string()));

    // The failure says how far the download got: which step, and the last write taken.
    let progress = failure.progress.as_ref().unwrap();
    assert!(progress.steps_completed > 0 && progress.steps_completed < progress.steps);
    assert_eq!(
        progress.failed_step.as_deref(),
        Some("start loading, object 3")
    );
    let writes = received.iter().filter(|line| is_write(line)).count();
    assert_eq!(progress.writes, writes);
    // The failing event itself reached the device, so it is the last write.
    assert_eq!(progress.last_write.as_ref(), Some(&load));
    let written = failure.written();
    assert!(written.starts_with("stopped in step "), "{written}");
    assert!(written.contains("(start loading, object 3)"), "{written}");
    assert!(
        written.contains(&format!("the last one {load}")),
        "{written}"
    );
    assert!(written.ends_with("needs a complete download, here or in ETS"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_never_reconnects_and_says_how_far_it_got() {
    let download = download_for(0x0021);
    let complete = download
        .plan(&mut SimulatedDevice::holding(&download), &strict())
        .unwrap();
    // The device drops the connection after 20 numbered frames, in the middle of the
    // procedure.
    let mut run = Run::with(SimulatedDevice::holding(&download));
    run.set_faults(Faults {
        forget_connection_after: Some(20),
        ..Faults::default()
    });
    let failure = run.download(&download, strict()).await.unwrap_err();
    assert!(
        failure.message.contains("the device closed the connection"),
        "{}",
        failure.message
    );
    // Exactly the first 21 requests went out (the connect and 20 numbered frames), then the
    // one that found the connection gone. No second connect, no repeated write.
    let received = run.received();
    assert_eq!(received, lines(&complete.requests[..21]));
    assert_eq!(
        received.iter().filter(|line| *line == "T_CONNECT").count(),
        1
    );
    let progress = failure.progress.as_ref().unwrap();
    let writes: Vec<&String> = received.iter().filter(|line| is_write(line)).collect();
    assert_eq!(progress.writes, writes.len());
    assert_eq!(
        progress
            .last_write
            .as_ref()
            .map(ToString::to_string)
            .as_ref(),
        writes.last().copied()
    );
    assert!(progress.failed_step.is_some());
    assert!(failure.written().contains("partly loaded"));

    // A failure before the first write says so.
    let mut run = Run::with(SimulatedDevice::holding(&download));
    run.set_faults(Faults {
        forget_connection_after: Some(2),
        ..Faults::default()
    });
    let failure = run.download(&download, strict()).await.unwrap_err();
    assert_eq!(failure.written(), "nothing was written to the device");
    assert_eq!(failure.progress.as_ref().unwrap().failed_step, None);
}

// ------------------------------------------------------------------------------------------
// Comparing over a bad link
// ------------------------------------------------------------------------------------------

/// Arms `faults` on the device when the comparison makes its `nth` memory read (counted
/// from 1), and passes every report on.
fn arm_at_read(bus: &SimBus, nth: usize, faults: Faults) -> impl FnMut(Report) + Send + use<> {
    let bus = bus.clone();
    let mut reads = 0;
    move |report: Report| {
        if let Report::Request(Request::MemoryRead { .. }) = report {
            reads += 1;
            if reads == nth {
                bus.with(DEVICE, |endpoint| endpoint.faults = faults);
            }
        }
    }
}

fn planned_bytes(download: &Download) -> usize {
    download
        .image
        .runs(0, 0x1_0000)
        .iter()
        .map(|run| run.data.len())
        .sum()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_drops_the_connection_mid_compare() {
    let download = download_for(0x0021);
    let bus = SimBus::new();
    bus.add(DEVICE, SimulatedDevice::holding(&download));
    let mut session = Session::new(bus.link(TOOL), DEVICE, fast());
    // From the second read on, the device has forgotten the connection: it answers the read
    // with a disconnect.
    let mut report = arm_at_read(
        &bus,
        2,
        Faults {
            forget_connection_after: Some(0),
            ..Faults::default()
        },
    );
    let comparison = ops::compare(
        &mut session,
        Arc::clone(&download),
        strict(),
        limits(),
        &mut report,
        &mut Cancel::never(),
    )
    .await
    .unwrap();
    session.finish().await;
    assert!(comparison.is_complete(), "{:?}", comparison.stopped_because);
    assert_eq!(comparison.reconnects, 1);
    assert_eq!(comparison.compared, planned_bytes(&download));
    assert_eq!(
        comparison.matching + comparison.differing,
        comparison.compared
    );
    // What was read before the disconnect was kept: the first read was made once, the second
    // twice.
    let received: Vec<String> = bus
        .with(DEVICE, |endpoint| lines(endpoint.received()))
        .unwrap();
    let reads: Vec<&String> = received
        .iter()
        .filter(|line| line.starts_with("MEM_READ"))
        .collect();
    assert_eq!(reads.iter().filter(|line| **line == reads[0]).count(), 1);
    assert!(!received.iter().any(|line| is_write(line)));
    assert_eq!(
        received.iter().filter(|line| *line == "T_CONNECT").count(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn four_transmissions_and_then_a_disconnect() {
    // What a lossy link did to a real comparison: one read needed four transmissions, its
    // answer came, and the device met the next read with a disconnect.
    let download = download_for(0x0021);
    let run = |limits: CompareLimits| {
        let download = Arc::clone(&download);
        async move {
            let bus = SimBus::new();
            bus.add(DEVICE, SimulatedDevice::holding(&download));
            let mut session = Session::new(bus.link(TOOL), DEVICE, fast());
            let mut notes = Vec::new();
            let mut arm = arm_at_read(
                &bus,
                2,
                Faults {
                    lose_requests: 3,
                    // The read that gets through is still taken; the one after it is not.
                    forget_connection_after: Some(1),
                    ..Faults::default()
                },
            );
            let mut report = |report: Report| {
                if let Report::Note(note) = &report {
                    notes.push(note.clone());
                }
                arm(report);
            };
            let comparison = ops::compare(
                &mut session,
                Arc::clone(&download),
                strict(),
                limits,
                &mut report,
                &mut Cancel::never(),
            )
            .await
            .unwrap();
            session.finish().await;
            (comparison, notes)
        }
    };

    // Repeating as the transport layer prescribes: the fourth transmission gets through, the
    // disconnect that follows costs one new connection, and the result is complete.
    let (comparison, notes) = run(limits()).await;
    assert!(comparison.is_complete(), "{:?}", comparison.stopped_because);
    assert_eq!(comparison.compared, planned_bytes(&download));
    assert_eq!(comparison.repetitions, 3);
    assert_eq!(comparison.reconnects, 1);
    assert!(notes.iter().any(|note| note.contains("transmission 4")));
    assert!(
        notes
            .iter()
            .any(|note| note.contains("the device closed the connection"))
    );

    // Watching for silence: once nothing has been heard for as long as a device keeps a
    // connection open, the read is not repeated into the old connection any more. Here that
    // is after two acknowledgement timeouts, so there is no fourth transmission.
    let impatient = CompareLimits {
        stale_after: Some(Duration::from_millis(90)),
        ..limits()
    };
    let (comparison, notes) = run(impatient).await;
    assert!(comparison.is_complete(), "{:?}", comparison.stopped_because);
    assert_eq!(comparison.compared, planned_bytes(&download));
    assert!(comparison.reconnects >= 1);
    assert!(!notes.iter().any(|note| note.contains("transmission 4")));
    assert!(
        notes
            .iter()
            .any(|note| note.contains("nothing heard from the device")
                || note.contains("silent for longer")),
        "{notes:?}"
    );
}

/// A link that loses every `outgoing`th frame it is given and every `incoming`th frame that
/// comes back.
struct Lossy<L> {
    inner: L,
    outgoing: u32,
    incoming: u32,
    sent: u32,
    received: u32,
    lost: u32,
}

impl<L: Link> Link for Lossy<L> {
    fn address(&self) -> IndividualAddress {
        self.inner.address()
    }

    async fn send(&mut self, device: IndividualAddress, tpdu: Tpdu) -> Result<(), LinkError> {
        self.sent += 1;
        if self.sent.is_multiple_of(self.outgoing) {
            self.lost += 1;
            return Ok(());
        }
        self.inner.send(device, tpdu).await
    }

    async fn receive(&mut self) -> Option<(IndividualAddress, Tpdu)> {
        loop {
            let frame = self.inner.receive().await?;
            self.received += 1;
            if self.received.is_multiple_of(self.incoming) {
                self.lost += 1;
                continue;
            }
            return Some(frame);
        }
    }

    async fn close(self) {
        self.inner.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_that_keeps_losing_frames_still_gives_a_full_result() {
    for (mask, download) in downloads() {
        // What a clean link finds.
        let mut clean = Run::with(SimulatedDevice::holding(&download));
        let expected = clean.compare(&download).await.unwrap();
        assert!(expected.is_complete());
        assert_eq!((expected.repetitions, expected.reconnects), (0, 0));

        for (outgoing, incoming) in [(7, 5), (4, 9), (3, 4)] {
            // The device repeats an answer that is not acknowledged, sooner than the tool
            // gives up waiting for one.
            let bus = SimBus::with_answer_timeout(Duration::from_millis(60));
            bus.add(DEVICE, SimulatedDevice::holding(&download));
            let link = Lossy {
                inner: bus.link(TOOL),
                outgoing,
                incoming,
                sent: 0,
                received: 0,
                lost: 0,
            };
            let mut session = Session::new(link, DEVICE, fast());
            let comparison = ops::compare(
                &mut session,
                Arc::clone(&download),
                strict(),
                limits(),
                &mut |_| {},
                &mut Cancel::never(),
            )
            .await
            .unwrap_or_else(|failure| {
                panic!(
                    "mask {mask:04X}, {outgoing}/{incoming}: {}",
                    failure.message
                )
            });
            let link = session.finish().await;
            let label = format!(
                "mask {mask:04X}, every {outgoing}th frame out and every {incoming}th back lost                  ({} in all): {} repetitions, {} reconnects, {} requests",
                link.lost, comparison.repetitions, comparison.reconnects, comparison.requests
            );
            println!("{label}");
            assert!(
                comparison.is_complete(),
                "{label}: {:?}",
                comparison.stopped_because
            );
            // The same findings as over the clean link, byte for byte.
            assert_eq!(comparison.compared, expected.compared, "{label}");
            assert_eq!(comparison.differences, expected.differences, "{label}");
            assert_eq!(comparison.found.kind, expected.found.kind, "{label}");
            // And the bad link shows in the counts: frames repeated by either side.
            assert!(link.lost > 5, "{label}");
            assert!(
                comparison.repetitions + comparison.reconnects > 0,
                "{label}"
            );
            let written = bus
                .with(DEVICE, |endpoint| {
                    lines(endpoint.received()).iter().any(|line| is_write(line))
                })
                .unwrap();
            assert!(!written, "{label}");
        }
    }
}

// ------------------------------------------------------------------------------------------
// A download that recovers from one failed exchange by reading
// ------------------------------------------------------------------------------------------

/// The recovery with timers to go with the short ones: an answer is given up 15 ms after an
/// acknowledged repetition and 60 ms after a first acknowledgement, both before the device's
/// idle time of 100 ms is over.
fn recovering() -> DownloadLimits {
    DownloadLimits {
        recoveries: 3,
        lost_answer_after: Some(Duration::from_millis(15)),
        unanswered_after: Some(Duration::from_millis(60)),
        reconnect_pause: Duration::from_millis(5),
    }
}

/// What a write is, for trying one of each: a load event by its number, any other property
/// write, a memory write. `None` for everything else.
fn write_kind(request: &Request, mask: u16) -> Option<String> {
    match request {
        Request::PropertyWrite {
            property: 5, data, ..
        } if data.len() == 10 => Some(format!("load event {}", data[0])),
        Request::PropertyWrite { .. } => Some("property".into()),
        // Mask 0701 takes its load events as memory writes to 0104.
        Request::MemoryWrite {
            address: 0x0104,
            data,
        } if mask == 0x0701 => Some(format!("load event {}", data[0] & 0x0F)),
        Request::MemoryWrite { .. } => Some("memory".into()),
        _ => None,
    }
}

/// A download over a clean link: its requests in order, and for each whether it was answered.
async fn clean_download(download: &Arc<Download>) -> Vec<(Request, bool)> {
    let mut run = Run::with(SimulatedDevice::holding(download));
    run.download(download, strict()).await.unwrap();
    let mut requests: Vec<(Request, bool)> = Vec::new();
    for report in &run.reports {
        match report {
            Report::Request(request) => requests.push((request.clone(), false)),
            Report::Response(_) => requests.last_mut().unwrap().1 = true,
            _ => {}
        }
    }
    requests
}

/// The first request of each kind of write in `requests`, as (position, kind, answered).
fn one_of_each(requests: &[(Request, bool)], mask: u16) -> Vec<(usize, String, bool)> {
    let mut seen = Vec::new();
    let mut picked = Vec::new();
    for (at, (request, answered)) in requests.iter().enumerate() {
        if let Some(kind) = write_kind(request, mask)
            && !seen.contains(&(kind.clone(), *answered))
        {
            seen.push((kind.clone(), *answered));
            picked.push((at, kind, *answered));
        }
    }
    picked
}

struct Recovered {
    result: Result<ops::Downloaded, Failure>,
    reports: Vec<Report>,
    /// What the device took, in order.
    received: Vec<String>,
    /// Bytes that differ from the download afterwards, by a comparison over a clean link.
    differing: Option<usize>,
}

impl Recovered {
    fn count(&self, wanted: impl Fn(&Report) -> bool) -> usize {
        self.reports.iter().filter(|report| wanted(report)).count()
    }

    fn recoveries(&self) -> usize {
        self.count(|report| matches!(report, Report::Recovering(_)))
    }

    fn reconnects(&self) -> usize {
        self.count(|report| matches!(report, Report::Reconnecting))
    }
}

/// Download to a device that keeps its answers to itself and gives a connection up after
/// 100 ms, with `faults` set on it when the request at position `at` is about to go out.
async fn download_with_fault(
    download: &Arc<Download>,
    clean: &[(Request, bool)],
    at: usize,
    faults: Faults,
) -> Recovered {
    download_with_faults(download, clean, &[at], faults, recovering()).await
}

/// `at` are positions in `clean`, the download as it goes over a clean link.
async fn download_with_faults(
    download: &Arc<Download>,
    clean: &[(Request, bool)],
    at: &[usize],
    faults: Faults,
    limits: DownloadLimits,
) -> Recovered {
    let bus = SimBus::new().with_idle_timeout(Duration::from_millis(100));
    bus.add(DEVICE, SimulatedDevice::holding(download));
    bus.with(DEVICE, |endpoint| {
        endpoint.faults.keeps_answers_to_itself = true
    });
    let mut session = Session::new(bus.link(TOOL), DEVICE, fast());
    let mut reports = Vec::new();
    let faulty = bus.clone();
    let mut sent = 0;
    let mut armed = 0;
    let result = ops::download(
        &mut session,
        Arc::clone(download),
        strict(),
        limits,
        &mut |report| {
            // Requests of a recovery are not counted: the next request of the download is the
            // next one of the clean run.
            if let Report::Request(request) = &report
                && clean.get(sent).is_some_and(|(next, _)| next == request)
            {
                if armed < at.len() && sent == at[armed] {
                    armed += 1;
                    faulty.with(DEVICE, |endpoint| {
                        endpoint.faults = Faults {
                            keeps_answers_to_itself: true,
                            verify_mode_ends_with_connection: endpoint
                                .faults
                                .verify_mode_ends_with_connection,
                            ..faults
                        };
                    });
                }
                sent += 1;
            }
            reports.push(report);
        },
        &mut Cancel::never(),
    )
    .await;
    session.finish().await;
    let received = bus
        .with(DEVICE, |endpoint| lines(endpoint.received()))
        .unwrap();
    let differing = if result.is_ok() {
        bus.with(DEVICE, |endpoint| endpoint.faults = Faults::default());
        let mut session = Session::new(bus.link(TOOL), DEVICE, fast());
        let comparison = ops::compare(
            &mut session,
            Arc::clone(download),
            strict(),
            limits_for_compare(),
            &mut |_| {},
            &mut Cancel::never(),
        )
        .await
        .unwrap();
        session.finish().await;
        Some(comparison.differing)
    } else {
        None
    };
    Recovered {
        result,
        reports,
        received,
        differing,
    }
}

fn limits_for_compare() -> CompareLimits {
    limits()
}

/// `received` without the requests of the recovery: what is left must be the download as it
/// goes over a clean link.
fn without_recovery_reads(received: &[String], clean: &[(Request, bool)]) -> Vec<String> {
    let mut clean = clean
        .iter()
        .map(|(request, _)| request.to_string())
        .peekable();
    received
        .iter()
        .filter(|line| {
            if clean.peek() == Some(*line) {
                clean.next();
                true
            } else {
                false
            }
        })
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_answer_to_a_write_does_not_stop_a_download() {
    for (mask, download) in downloads() {
        let clean = clean_download(&download).await;
        let plan: Vec<String> = clean
            .iter()
            .map(|(request, _)| request.to_string())
            .collect();
        for (at, kind, answered) in one_of_each(&clean, mask) {
            if !answered {
                // A write without an answer has none to lose.
                continue;
            }
            let label = format!("mask {mask:04X}, {kind} ({})", plan[at]);
            // The answer alone is lost: the request was acknowledged at the first go.
            // Then acknowledgement and answer together: the repetition is acknowledged.
            for (what, faults) in [
                (
                    "answer lost",
                    Faults {
                        lose_answers_on_the_way: 1,
                        ..Faults::default()
                    },
                ),
                (
                    "acknowledgement and answer lost",
                    Faults {
                        lose_exchanges: 1,
                        ..Faults::default()
                    },
                ),
            ] {
                let label = format!("{label}, {what}");
                let outcome = download_with_fault(&download, &clean, at, faults).await;
                let done = outcome
                    .result
                    .as_ref()
                    .unwrap_or_else(|failure| panic!("{label}: {}", failure.message));
                // One recovery, on the connection at hand, and the device holds it all.
                assert_eq!(done.recoveries, 1, "{label}");
                assert_eq!(
                    (outcome.recoveries(), outcome.reconnects()),
                    (1, 0),
                    "{label}"
                );
                assert_eq!(outcome.differing, Some(0), "{label}");
                // The device took the download as over a clean link, the write once, and
                // one read more: the one that showed what the write had left.
                assert_eq!(
                    without_recovery_reads(&outcome.received, &clean),
                    plan,
                    "{label}"
                );
                assert_eq!(outcome.received.len(), plan.len() + 1, "{label}");
                let read = &outcome.received[at + 1];
                assert!(
                    read.starts_with("MEM_READ") || read.starts_with("PROP_READ"),
                    "{label}: {read}"
                );
                assert_eq!(
                    outcome
                        .received
                        .iter()
                        .filter(|line| **line == plan[at])
                        .count(),
                    plan.iter().filter(|line| **line == plan[at]).count(),
                    "{label}: the write went out once"
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_acknowledgement_alone_needs_no_recovery() {
    for (mask, download) in downloads() {
        let clean = clean_download(&download).await;
        for (at, kind, _) in one_of_each(&clean, mask) {
            let outcome = download_with_fault(
                &download,
                &clean,
                at,
                Faults {
                    lose_acks: 1,
                    ..Faults::default()
                },
            )
            .await;
            let label = format!("mask {mask:04X}, {kind}");
            assert!(outcome.result.is_ok(), "{label}");
            assert_eq!(
                (outcome.recoveries(), outcome.reconnects()),
                (0, 0),
                "{label}"
            );
            assert_eq!(outcome.differing, Some(0), "{label}");
        }
    }
}

/// A write the device never took, because the request did not arrive in four transmissions or
/// because the device had dropped the connection: on a new connection the read shows that,
/// and only then is the write sent again. An event whose effect the load state cannot show
/// stops the download instead.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_that_was_never_taken_is_sent_again_only_after_a_read() {
    for (mask, download) in downloads() {
        let clean = clean_download(&download).await;
        let plan: Vec<String> = clean
            .iter()
            .map(|(request, _)| request.to_string())
            .collect();
        for (at, kind, _) in one_of_each(&clean, mask) {
            for (what, faults) in [
                (
                    "request lost four times",
                    Faults {
                        lose_requests: 4,
                        ..Faults::default()
                    },
                ),
                (
                    "connection dropped by the device",
                    Faults {
                        forget_connection_after: Some(0),
                        ..Faults::default()
                    },
                ),
            ] {
                let label = format!("mask {mask:04X}, {kind} ({}), {what}", plan[at]);
                let outcome = download_with_fault(&download, &clean, at, faults).await;
                assert_eq!(
                    (outcome.recoveries(), outcome.reconnects()),
                    (1, 1),
                    "{label}"
                );
                // The load state tells of a load, a completed load and an unload; memory reads
                // back. A property may not (a write-only one reads back as nothing).
                let telling = ["load event 1", "load event 2", "load event 4", "memory"]
                    .contains(&kind.as_str())
                    || (kind == "property" && outcome.result.is_ok());
                match &outcome.result {
                    Ok(done) => {
                        assert!(telling, "{label}: went on without being able to tell");
                        assert_eq!(done.recoveries, 1, "{label}");
                        assert_eq!(outcome.differing, Some(0), "{label}");
                        // Whatever the read found, the device took the write once.
                        assert_eq!(
                            outcome
                                .received
                                .iter()
                                .filter(|line| **line == plan[at])
                                .count(),
                            plan.iter().filter(|line| **line == plan[at]).count(),
                            "{label}"
                        );
                    }
                    Err(failure) => {
                        assert!(!telling, "{label}: {}", failure.message);
                        assert!(
                            failure.message.contains("recovering from that")
                                && (failure.message.contains("cannot tell") || kind == "property"),
                            "{label}: {}",
                            failure.message
                        );
                        // Not sent again on a guess.
                        assert_eq!(
                            outcome
                                .received
                                .iter()
                                .filter(|line| **line == plan[at])
                                .count()
                                + 1,
                            plan[..=at].iter().filter(|line| **line == plan[at]).count(),
                            "{label}"
                        );
                    }
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_back_that_shows_something_else_stops_the_download() {
    for (mask, download) in downloads() {
        let clean = clean_download(&download).await;
        let plan: Vec<String> = clean
            .iter()
            .map(|(request, _)| request.to_string())
            .collect();
        for (at, kind, answered) in one_of_each(&clean, mask) {
            // The device acknowledges the write and does nothing with it. Where the write
            // has an answer, its absence starts the recovery; the read then shows that the
            // device does not hold what was written.
            if !answered || !["load event 1", "load event 2", "memory"].contains(&kind.as_str()) {
                continue;
            }
            let label = format!("mask {mask:04X}, {kind} ({})", plan[at]);
            let outcome = download_with_fault(
                &download,
                &clean,
                at,
                Faults {
                    ignore_writes: 1,
                    ..Faults::default()
                },
            )
            .await;
            let failure = outcome
                .result
                .as_ref()
                .expect_err(&format!("{label}: must not go on"));
            assert!(
                failure.message.contains("recovering from that")
                    && (failure.message.contains("does not hold what was written")
                        || failure.message.contains("not what the event leads to")),
                "{label}: {}",
                failure.message
            );
            assert_eq!(outcome.recoveries(), 1, "{label}");
            // The write went out once, and nothing of the download after it.
            assert_eq!(
                outcome
                    .received
                    .iter()
                    .filter(|line| **line == plan[at])
                    .count(),
                0,
                "{label}: the device ignored it, so it never counted it as taken"
            );
            assert!(
                !outcome
                    .received
                    .iter()
                    .any(|line| *line == plan[at + 1] && is_write(line)),
                "{label}"
            );
            assert!(failure.progress.is_some(), "{label}");
        }
    }
}

/// Echoing of memory writes is set for a connection. A download that has to open a new one
/// looks whether the device still has the setting and puts it back before going on.
#[tokio::test(flavor = "multi_thread")]
async fn the_setting_of_the_connection_is_put_back_on_a_new_one() {
    let mut tried = 0;
    for (mask, download) in downloads() {
        let clean = clean_download(&download).await;
        let plan: Vec<String> = clean
            .iter()
            .map(|(request, _)| request.to_string())
            .collect();
        let is_setting = |line: &String| line.starts_with("PROP_WRITE obj=0 pid=14 ");
        let Some(set_at) = plan.iter().position(is_setting) else {
            continue;
        };
        // An echoed memory write after the setting was made.
        let Some(at) = clean
            .iter()
            .enumerate()
            .position(|(at, (request, answered))| {
                at > set_at && *answered && matches!(request, Request::MemoryWrite { .. })
            })
        else {
            continue;
        };
        tried += 1;
        let bus = SimBus::new().with_idle_timeout(Duration::from_millis(100));
        bus.add(DEVICE, SimulatedDevice::holding(&download));
        bus.with(DEVICE, |endpoint| {
            endpoint.faults.verify_mode_ends_with_connection = true;
        });
        let mut session = Session::new(bus.link(TOOL), DEVICE, fast());
        let faulty = bus.clone();
        let mut sent = 0;
        let mut reconnects = 0;
        let result = ops::download(
            &mut session,
            Arc::clone(&download),
            strict(),
            recovering(),
            &mut |report| match &report {
                Report::Request(request)
                    if clean.get(sent).is_some_and(|(next, _)| next == request) =>
                {
                    if sent == at {
                        faulty.with(DEVICE, |endpoint| {
                            endpoint.faults.forget_connection_after = Some(0);
                        });
                    }
                    sent += 1;
                }
                Report::Reconnecting => reconnects += 1,
                _ => {}
            },
            &mut Cancel::never(),
        )
        .await;
        session.finish().await;
        let done = result.unwrap_or_else(|failure| panic!("mask {mask:04X}: {}", failure.message));
        assert_eq!((done.recoveries, reconnects), (1, 1), "mask {mask:04X}");
        let received = bus
            .with(DEVICE, |endpoint| lines(endpoint.received()))
            .unwrap();
        // Read on the new connection, found gone, written once more.
        assert_eq!(
            received.iter().filter(|line| is_setting(line)).count(),
            plan.iter().filter(|line| is_setting(line)).count() + 1,
            "mask {mask:04X}: {received:#?}"
        );
        let reopened = received
            .iter()
            .rposition(|line| line == "T_CONNECT")
            .unwrap();
        assert_eq!(
            received[reopened + 1..reopened + 5]
                .iter()
                .map(|line| line.split(' ').next().unwrap())
                .collect::<Vec<_>>(),
            [
                "DEVICE_DESCRIPTOR_READ",
                "AUTHORIZE_REQUEST",
                "PROP_READ",
                "PROP_WRITE"
            ],
            "mask {mask:04X}"
        );
    }
    assert!(
        tried > 0,
        "no download here sets the device control property"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_recovers_three_times_and_then_stops() {
    let download = download_for(0x0705);
    let clean = clean_download(&download).await;
    let answered: Vec<usize> = clean
        .iter()
        .enumerate()
        .filter(|(_, (request, answered))| *answered && write_kind(request, 0x0705).is_some())
        .map(|(at, _)| at)
        .collect();
    assert!(answered.len() >= 4, "{}", answered.len());
    let lost = Faults {
        lose_answers_on_the_way: 1,
        ..Faults::default()
    };
    // Three lost answers: recovered from, each on the connection at hand.
    let outcome = download_with_faults(&download, &clean, &answered[..3], lost, recovering()).await;
    assert_eq!(outcome.result.as_ref().unwrap().recoveries, 3);
    assert_eq!(outcome.differing, Some(0));
    // A fourth: the download stops there, as it would have at the first without recovery.
    let outcome = download_with_faults(&download, &clean, &answered[..4], lost, recovering()).await;
    let failure = outcome.result.as_ref().unwrap_err();
    assert_eq!(outcome.recoveries(), 3);
    assert!(
        failure.message.contains("its answer did not arrive"),
        "{}",
        failure.message
    );
    // And with no recovery allowed, at the first.
    let outcome = download_with_faults(
        &download,
        &clean,
        &answered[..1],
        lost,
        DownloadLimits::none(),
    )
    .await;
    assert!(outcome.result.is_err());
    assert_eq!(outcome.recoveries(), 0);
}

// ------------------------------------------------------------------------------------------
// A device that is not loaded
// ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_unloaded_device_is_named_and_its_memory_left_alone() {
    for (mask, download) in downloads() {
        // What a download leaves that stopped after its unloads: nothing loaded, and memory
        // reads answered with no data.
        let mut device = SimulatedDevice::holding(&download);
        for object in 1..=3 {
            device.set_load_state(object, 0);
        }
        device.refuse_memory_reads(true);
        let mut run = Run::with(device);
        let comparison = run.compare(&download).await.unwrap();
        assert_eq!(
            comparison.not_loaded.as_deref(),
            Some(
                "the device holds no application: its address table, association table and \
                 application are unloaded; it needs a full download"
            ),
            "mask {mask:04X}"
        );
        // Said in the checking phase, on the one connection, without a single read of the
        // memory that cannot be had.
        assert_eq!((comparison.reconnects, comparison.compared), (0, 0));
        assert!(comparison.is_complete() && comparison.stopped_because.is_none());
        assert!(comparison.planned > 0 && comparison.differences.is_empty());
        let received = run.received();
        assert_eq!(
            received.iter().filter(|line| *line == "T_CONNECT").count(),
            1,
            "mask {mask:04X}"
        );
        assert!(
            !run.reports
                .iter()
                .any(|report| matches!(report, Report::Phase("Reading the device"))),
            "mask {mask:04X}"
        );
        assert!(
            run.notes()
                .iter()
                .any(|note| note.contains("its memory is not compared")),
            "mask {mask:04X}"
        );
        // The three states were asked for the way the mask has them.
        let states: Vec<&String> = received
            .iter()
            .filter(|line| line.contains("pid=5 ") || line.starts_with("MEM_READ B6E"))
            .collect();
        assert!(states.len() >= 3, "mask {mask:04X}: {received:?}");

        // Left half-way: the tables loaded, the application still being loaded.
        let mut device = SimulatedDevice::holding(&download);
        device.set_load_state(3, 2);
        let mut run = Run::with(device);
        let comparison = run.compare(&download).await.unwrap();
        assert_eq!(
            comparison.not_loaded.as_deref(),
            Some(
                "the device is partly loaded: its address table is loaded, its association \
                 table is loaded, its application is being loaded; it needs a full download"
            ),
            "mask {mask:04X}"
        );
        assert_eq!(comparison.compared, 0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_answered_with_no_data_is_a_refusal_not_a_lost_connection() {
    // The device says it is loaded and still gives no memory: the first read is answered
    // with a count of zero. That is the device's answer; no new connection is tried for it.
    let download = download_for(0x0021);
    let mut device = SimulatedDevice::holding(&download);
    device.refuse_memory_reads(true);
    let mut run = Run::with(device);
    let comparison = run.compare(&download).await.unwrap();
    assert_eq!(comparison.not_loaded, None);
    assert!(!comparison.is_complete());
    assert_eq!((comparison.reconnects, comparison.compared), (0, 0));
    let why = comparison.stopped_because.as_deref().unwrap();
    assert!(
        why.contains("does not give its memory at 0100") && why.contains("no data"),
        "{why}"
    );
    let received = run.received();
    assert_eq!(
        received
            .iter()
            .filter(|line| line.starts_with("MEM_READ 01"))
            .collect::<Vec<_>>(),
        ["MEM_READ 0100 1"]
    );
    assert_eq!(
        received.iter().filter(|line| *line == "T_CONNECT").count(),
        1
    );
    assert_eq!(comparison.unread.len(), 1.max(comparison.unread.len()));
    assert_eq!(
        comparison
            .unread
            .iter()
            .map(|stretch| stretch.length)
            .sum::<usize>(),
        comparison.planned
    );
}

// ------------------------------------------------------------------------------------------
// An answer lost together with its acknowledgement
// ------------------------------------------------------------------------------------------

/// A link that counts what passes: the acknowledgements the tool sends and the numbered
/// frames it is given.
struct Counting<L> {
    inner: L,
    acknowledgements_sent: u32,
    data_received: u32,
    disconnects_received: u32,
}

impl<L: Link> Counting<L> {
    fn new(inner: L) -> Self {
        Self {
            inner,
            acknowledgements_sent: 0,
            data_received: 0,
            disconnects_received: 0,
        }
    }
}

impl<L: Link> Link for Counting<L> {
    fn address(&self) -> IndividualAddress {
        self.inner.address()
    }

    async fn send(&mut self, device: IndividualAddress, tpdu: Tpdu) -> Result<(), LinkError> {
        if matches!(tpdu, Tpdu::Ack { .. }) {
            self.acknowledgements_sent += 1;
        }
        self.inner.send(device, tpdu).await
    }

    async fn receive(&mut self) -> Option<(IndividualAddress, Tpdu)> {
        let frame = self.inner.receive().await?;
        match frame.1 {
            Tpdu::Connected { .. } => self.data_received += 1,
            Tpdu::Disconnect => self.disconnects_received += 1,
            _ => {}
        }
        Some(frame)
    }

    async fn close(self) {
        self.inner.close().await;
    }
}

/// The device as one of mask 0021 looked on a lossy link: of every `every`th exchange both
/// its acknowledgement and its answer are lost, it acknowledges the repeated request without
/// answering again, it does not repeat its answer, and it closes the connection when nothing
/// has reached it for the idle time (100 ms here, for six seconds).
fn quiet_device(download: &Download, every: u32) -> SimBus {
    let bus = SimBus::new().with_idle_timeout(Duration::from_millis(100));
    bus.add(DEVICE, SimulatedDevice::holding(download));
    bus.with(DEVICE, |endpoint| {
        endpoint.faults = Faults {
            lose_exchange_every: Some(every),
            keeps_answers_to_itself: true,
            ..Faults::default()
        };
    });
    bus
}

fn connects(bus: &SimBus) -> usize {
    bus.with(DEVICE, |endpoint| {
        endpoint
            .received()
            .iter()
            .filter(|request| **request == Request::Connect)
            .count()
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_answer_is_asked_for_again_on_the_same_connection() {
    for (mask, download) in downloads() {
        let mut clean = Run::with(SimulatedDevice::holding(&download));
        let expected = clean.compare(&download).await.unwrap();

        let bus = quiet_device(&download, 4);
        let mut session = Session::new(Counting::new(bus.link(TOOL)), DEVICE, fast());
        let mut reports = Vec::new();
        let comparison = ops::compare(
            &mut session,
            Arc::clone(&download),
            strict(),
            CompareLimits {
                // A fraction of the device's idle time, as one second is of six.
                lost_answer_after: Some(Duration::from_millis(15)),
                stale_after: Some(Duration::from_millis(100)),
                ..limits()
            },
            &mut |report| reports.push(report),
            &mut Cancel::never(),
        )
        .await
        .unwrap_or_else(|failure| panic!("mask {mask:04X}: {}", failure.message));
        let link = session.finish().await;
        let label = format!(
            "mask {mask:04X}: {} requests, {} repeated frames, {} answers asked for again, {} \
             new connections",
            comparison.requests,
            comparison.repetitions,
            comparison.re_requests,
            comparison.reconnects
        );
        println!("{label}");

        // Complete, with the findings of a clean link, on the one connection it opened.
        assert!(comparison.is_complete(), "{label}");
        assert_eq!(comparison.differences, expected.differences, "{label}");
        assert_eq!(comparison.compared, expected.compared, "{label}");
        assert_eq!(comparison.reconnects, 0, "{label}");
        assert_eq!(connects(&bus), 1, "{label}");
        assert_eq!(link.disconnects_received, 0, "{label}");
        // Every fourth exchange was lost and asked for again: once each, after one
        // repetition each.
        assert!(comparison.re_requests >= 3, "{label}");
        assert_eq!(comparison.repetitions, comparison.re_requests, "{label}");
        assert_eq!(comparison.requests, expected.requests, "{label}");
        let asked_again: u32 = reports
            .iter()
            .map(|report| match report {
                Report::AskedAgain(count) => *count,
                _ => 0,
            })
            .sum();
        assert_eq!(asked_again, comparison.re_requests, "{label}");
        // One acknowledgement for every answer that arrived, and one for every answer taken
        // as lost.
        assert_eq!(
            link.acknowledgements_sent,
            link.data_received + comparison.re_requests,
            "{label}"
        );
        // The device took each lost read twice (and the disconnect at the end), and nothing
        // was written.
        let received = bus
            .with(DEVICE, |endpoint| lines(endpoint.received()))
            .unwrap();
        assert_eq!(
            received.len(),
            comparison.requests + comparison.re_requests as usize + 1,
            "{label}"
        );
        assert_eq!(received.last().unwrap(), "T_DISCONNECT", "{label}");
        assert!(!received.iter().any(|line| is_write(line)), "{label}");
        assert!(
            reports.iter().any(
                |report| matches!(report, Report::Note(note) if note.contains("taking the answer as lost"))
            ),
            "{label}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn asking_again_in_vain_falls_back_to_a_new_connection() {
    // From the first memory read on every exchange is lost, until a new connection is
    // opened: the read is asked for again once, that is lost as well, and then the
    // comparison does what it does about any other trouble.
    let download = download_for(0x0021);
    let bus = SimBus::new().with_idle_timeout(Duration::from_millis(100));
    bus.add(DEVICE, SimulatedDevice::holding(&download));
    let mut session = Session::new(Counting::new(bus.link(TOOL)), DEVICE, fast());
    let faulty = bus.clone();
    let mut armed = false;
    let comparison = ops::compare(
        &mut session,
        Arc::clone(&download),
        strict(),
        CompareLimits {
            lost_answer_after: Some(Duration::from_millis(15)),
            ..limits()
        },
        &mut |report| {
            let faults = match report {
                Report::Request(Request::MemoryRead { .. }) if !armed => Faults {
                    lose_exchange_every: Some(1),
                    keeps_answers_to_itself: true,
                    ..Faults::default()
                },
                Report::Reconnecting => Faults::default(),
                _ => return,
            };
            armed = true;
            faulty.with(DEVICE, |endpoint| endpoint.faults = faults);
        },
        &mut Cancel::never(),
    )
    .await
    .unwrap();
    session.finish().await;
    assert!(comparison.is_complete());
    assert_eq!((comparison.re_requests, comparison.reconnects), (1, 1));
    // The findings are those of a clean link.
    let mut clean = Run::with(SimulatedDevice::holding(&download));
    let expected = clean.compare(&download).await.unwrap();
    assert_eq!(comparison.differences, expected.differences);
    assert_eq!(comparison.compared, expected.compared);
    assert_eq!(connects(&bus), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_asking_again_each_lost_answer_costs_a_connection() {
    // The same device, and a comparison that only knows the new connection: this is what
    // was seen on the bus. The losses start with the first memory read, so that they fall on
    // the reads and not, time and again, on the opening of each new connection.
    let download = download_for(0x0021);
    let bus = SimBus::new().with_idle_timeout(Duration::from_millis(100));
    bus.add(DEVICE, SimulatedDevice::holding(&download));
    let mut session = Session::new(Counting::new(bus.link(TOOL)), DEVICE, fast());
    let mut reports = Vec::new();
    let faulty = bus.clone();
    let mut armed = false;
    let comparison = ops::compare(
        &mut session,
        Arc::clone(&download),
        strict(),
        limits(),
        &mut |report| {
            if matches!(report, Report::Request(Request::MemoryRead { .. })) && !armed {
                armed = true;
                faulty.with(DEVICE, |endpoint| {
                    endpoint.faults = Faults {
                        lose_exchange_every: Some(5),
                        keeps_answers_to_itself: true,
                        ..Faults::default()
                    };
                });
            }
            reports.push(report);
        },
        &mut Cancel::never(),
    )
    .await
    .unwrap();
    let link = session.finish().await;
    assert!(comparison.is_complete());
    assert_eq!(comparison.re_requests, 0);
    assert!(comparison.reconnects >= 3, "{}", comparison.reconnects);
    assert_eq!(connects(&bus), 1 + comparison.reconnects as usize);
    // The repetition is acknowledged, no answer follows, and the device closes the
    // connection when its idle time is up.
    assert_eq!(link.disconnects_received, comparison.reconnects);
    assert_eq!(link.acknowledgements_sent, link.data_received);
    assert!(reports.iter().any(
        |report| matches!(report, Report::Note(note) if note.contains("the device closed the connection"))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_without_recovery_does_not_guess_about_a_lost_answer() {
    for (mask, download) in downloads() {
        let complete = download
            .plan(&mut SimulatedDevice::holding(&download), &strict())
            .unwrap();
        // Late enough for writes to have gone out.
        let bus = quiet_device(&download, 12);
        let mut session = Session::new(Counting::new(bus.link(TOOL)), DEVICE, fast());
        // A download that is not allowed to recover: answers are not taken as lost.
        let mut reports = Vec::new();
        let failure = ops::download(
            &mut session,
            Arc::clone(&download),
            strict(),
            DownloadLimits::none(),
            &mut |report| reports.push(report),
            &mut Cancel::never(),
        )
        .await
        .unwrap_err();
        let link = session.finish().await;
        assert!(
            failure.message.contains("the device closed the connection"),
            "mask {mask:04X}: {}",
            failure.message
        );
        // No acknowledgement of anything unseen, no request made twice, no new connection.
        assert_eq!(link.acknowledgements_sent, link.data_received, "{mask:04X}");
        assert!(
            !reports
                .iter()
                .any(|report| matches!(report, Report::AskedAgain(_))),
            "{mask:04X}"
        );
        let received = bus
            .with(DEVICE, |endpoint| lines(endpoint.received()))
            .unwrap();
        assert_eq!(
            received,
            lines(&complete.requests[..received.len()]),
            "mask {mask:04X}: what the device took is the plan up to where it stopped"
        );
        assert_eq!(connects(&bus), 1, "{mask:04X}");
        assert!(failure.progress.is_some(), "{mask:04X}");
    }
}

/// The total a comparison tells: what it has sent and the reads to come, corrected when
/// something is added, and in the end what it sent.
#[tokio::test(flavor = "multi_thread")]
async fn a_comparison_says_how_many_requests_it_expects() {
    let totals = |reports: &[Report]| -> (Vec<usize>, usize) {
        let expected = reports
            .iter()
            .filter_map(|report| match report {
                Report::Expected(total) => Some(*total),
                _ => None,
            })
            .collect();
        let sent = reports
            .iter()
            .filter(|report| matches!(report, Report::Request(_)))
            .count();
        (expected, sent)
    };
    for (mask, download) in downloads() {
        // A clean link: one total, told once the pre-check is through, and it holds.
        let mut run = Run::with(SimulatedDevice::holding(&download));
        let comparison = run.compare(&download).await.unwrap();
        let (expected, sent) = totals(&run.reports);
        assert_eq!(expected, [sent], "mask {mask:04X}");
        assert_eq!(comparison.requests, sent, "mask {mask:04X}");
        // Its reads, not the download's requests.
        let plan = download
            .plan(&mut SimulatedDevice::holding(&download), &strict())
            .unwrap();
        let reads = ops::planned_reads(download.variant(comparison.found.kind)).len();
        assert!(
            sent >= reads && sent != plan.requests.len(),
            "mask {mask:04X}"
        );

        // A new connection on the way adds its requests to the total.
        let mut run = Run::with(SimulatedDevice::holding(&download));
        run.set_faults(Faults {
            forget_connection_after: Some(sent as u32 - 3),
            ..Faults::default()
        });
        let comparison = run.compare(&download).await.unwrap();
        assert_eq!(comparison.reconnects, 1, "mask {mask:04X}");
        let (expected, sent_now) = totals(&run.reports);
        assert!(sent_now > sent && expected.len() > 1, "mask {mask:04X}");
        assert!(expected.is_sorted(), "mask {mask:04X}: {expected:?}");
        assert_eq!(expected.last(), Some(&sent_now), "mask {mask:04X}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_comparison_that_has_to_give_up_returns_what_it_has() {
    let download = download_for(0x0021);
    let compare = |limits: CompareLimits| {
        let download = Arc::clone(&download);
        async move {
            let bus = SimBus::new();
            bus.add(DEVICE, SimulatedDevice::holding(&download));
            let mut session = Session::new(bus.link(TOOL), DEVICE, fast());
            // The device takes the third read and then nothing more.
            let mut report = arm_at_read(
                &bus,
                3,
                Faults {
                    silent_after: Some(1),
                    ..Faults::default()
                },
            );
            let comparison = ops::compare(
                &mut session,
                Arc::clone(&download),
                strict(),
                limits,
                &mut report,
                &mut Cancel::never(),
            )
            .await
            .unwrap();
            session.finish().await;
            comparison
        }
    };

    let planned = planned_bytes(&download);
    let comparison = compare(limits()).await;
    assert!(!comparison.is_complete());
    assert_eq!(comparison.planned, planned);
    // Three reads went through before the device fell silent.
    assert!(comparison.compared > 0 && comparison.compared <= 36);
    assert_eq!(
        comparison.matching + comparison.differing,
        comparison.compared
    );
    let unread: usize = comparison.unread.iter().map(|stretch| stretch.length).sum();
    assert_eq!(comparison.compared + unread, planned);
    // The differences found are within what was read, the unread stretches behind them.
    let first_unread = comparison.unread[0].address;
    assert!(
        comparison
            .differences
            .iter()
            .all(|difference| difference.address < first_unread)
    );
    assert!(
        comparison
            .unread
            .windows(2)
            .all(|pair| pair[0].address < pair[1].address)
    );
    let reason = comparison.stopped_because.as_deref().unwrap();
    assert!(reason.contains("failed 5 times in a row"), "{reason}");
    assert!(reason.contains("MEM_READ"), "{reason}");
    // Five failures: the read itself, then four connections that got nowhere.
    assert_eq!(comparison.reconnects, 4);
    assert!(comparison.repetitions >= 15);

    // A budget of new connections for the whole comparison ends it as well.
    let tight = CompareLimits {
        attempts: 50,
        reconnects: 2,
        ..limits()
    };
    let comparison = compare(tight).await;
    assert!(!comparison.is_complete());
    assert_eq!(comparison.reconnects, 2);
    let reason = comparison.stopped_because.as_deref().unwrap();
    assert!(
        reason.contains("2 new connections were not enough"),
        "{reason}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trouble_during_the_pre_check_starts_it_again() {
    let download = download_for(0x0705);
    // The device drops the connection after three numbered frames: in the pre-check.
    let mut run = Run::with(SimulatedDevice::holding(&download));
    run.set_faults(Faults {
        forget_connection_after: Some(3),
        ..Faults::default()
    });
    let comparison = run.compare(&download).await.unwrap();
    assert!(comparison.is_complete());
    assert_eq!(comparison.reconnects, 1);
    assert_eq!(
        run.received()
            .iter()
            .filter(|line| *line == "T_CONNECT")
            .count(),
        2
    );
    // A check that fails is not trouble with the connection: no second attempt.
    let mut other = SimulatedDevice::holding(&download);
    other.set_property(0, 12, 1, &[0x00, 0x01]);
    let mut run = Run::with(other);
    let failure = run.compare(&download).await.unwrap_err();
    assert_eq!(failure.check, Some("manufacturer"));
    assert_eq!(
        run.received()
            .iter()
            .filter(|line| *line == "T_CONNECT")
            .count(),
        1
    );
    assert_eq!(failure.written(), "nothing was written to the device");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_of_another_mask_is_refused() {
    let download = download_for(0x0705);
    let mut run = Run::with(SimulatedDevice::new(0x0701));
    for compare in [false, true] {
        let failure = if compare {
            run.compare(&download).await.unwrap_err()
        } else {
            run.download(&download, strict()).await.unwrap_err()
        };
        assert_eq!(failure.check, Some("mask"));
        assert_eq!(
            failure.message,
            "mask check failed: the device has mask 0701, the application needs 0705"
        );
        assert_eq!(
            run.received(),
            ["T_CONNECT", "DEVICE_DESCRIPTOR_READ", "T_DISCONNECT"]
        );
        run.bus.with(DEVICE, |endpoint| endpoint.clear_received());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pre_check_stops_before_anything_is_written() {
    let download = download_for(0x0705);
    /// The check, what makes the device fail it, and the message.
    type Case<'a> = (&'a str, &'a dyn Fn(&mut SimulatedDevice), &'a str);
    let cases: [Case<'_>; 3] = [
        (
            "manufacturer",
            &|device| device.set_property(0, 12, 1, &[0x00, 0x01]),
            "manufacturer check failed: the device reports 0001, the application is for 00FA",
        ),
        (
            "application",
            &|device| device.set_property(3, 13, 1, &[0x00, 0xFA, 0x03, 0x01, 0x07]),
            "application check failed: the device holds 00FA/0301 version 07, the download is \
             for 00FA/0301 version 01",
        ),
        (
            "access",
            &|device| {
                device
                    .replay(
                        &["AUTHORIZE_REQUEST FFFFFFFF".parse().unwrap()],
                        &["AUTHORIZE_RESPONSE level=3".parse().unwrap()],
                    )
                    .unwrap();
            },
            "access check failed: access level 3 is not sufficient",
        ),
    ];
    for (check, change, message) in cases {
        for compare in [false, true] {
            let mut device = SimulatedDevice::holding(&download);
            change(&mut device);
            let mut run = Run::with(device);
            let failure = if compare {
                run.compare(&download).await.unwrap_err()
            } else {
                run.download(&download, strict()).await.unwrap_err()
            };
            assert_eq!(failure.check, Some(check), "{}", failure.message);
            assert_eq!(failure.message, message);
            let received = run.received();
            assert!(!received.iter().any(|line| is_write(line)), "{received:?}");
            assert_eq!(received.last().unwrap(), "T_DISCONNECT");
        }
    }

    // The hardware identification, on the BCU 2 product with the byte made part of the image.
    let program = ApplicationProgram::parse(&knxide_testkit::project::BCU2_PRODUCT.replace(
        "<Mask>/wAAAAAAAAAAAAAAAAD///8AAAAAAA==</Mask>",
        "<Mask>/wAAAAAAAAAAAAAA/wD///8AAAAAAA==</Mask>",
    ))
    .unwrap();
    let device = DeviceData::parse(knxide_testkit::project::BCU2_DEVICE).unwrap();
    let addresses = GroupAddresses::parse_list(knxide_testkit::project::BCU2_ADDRESSES).unwrap();
    let download = Arc::new(Download::prepare(&program, &device, &addresses).unwrap());
    let mut other = SimulatedDevice::holding(&download);
    other.set_property(0, 17, 1, &[0x42]);
    let mut run = Run::with(other);
    let failure = run.download(&download, strict()).await.unwrap_err();
    assert_eq!(failure.check, Some("hardware"));
    assert_eq!(
        failure.message,
        "hardware check failed: the device reports 42, the application is for 00"
    );
    assert!(!run.received().iter().any(|line| is_write(line)));
    let failure = run.compare(&download).await.unwrap_err();
    assert_eq!(failure.check, Some("hardware"));
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_at_the_address() {
    let download = download_for(0x0021);
    let bus = SimBus::new();
    let mut session = Session::new(bus.link(TOOL), DEVICE, fast());
    let failure = ops::download(
        &mut session,
        download,
        strict(),
        DownloadLimits::none(),
        &mut |_| {},
        &mut Cancel::never(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.message, "no device answers at 1.1.9");
    assert_eq!(failure.requests, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_stops_at_the_next_request() {
    let download = download_for(0x0705);
    let bus = SimBus::new();
    bus.add(DEVICE, SimulatedDevice::holding(&download));
    let mut session = Session::new(bus.link(TOOL), DEVICE, fast());
    let (switch, mut cancel) = Cancel::new();
    let mut seen = 0;
    let mut report = move |report: Report| {
        if matches!(report, Report::Request(_)) {
            seen += 1;
            if seen == 10 {
                switch.cancel();
            }
        }
    };
    let failure = ops::download(
        &mut session,
        download,
        strict(),
        DownloadLimits::none(),
        &mut report,
        &mut cancel,
    )
    .await
    .unwrap_err();
    session.finish().await;
    assert!(failure.cancelled);
    let received = bus
        .with(DEVICE, |endpoint| endpoint.received().len())
        .unwrap();
    // Ten requests at most reached the device, then the disconnect.
    assert!(received <= 11, "{received}");
}

// ------------------------------------------------------------------------------------------
// Recorded downloads
// ------------------------------------------------------------------------------------------

const VARIABLE: &str = "KNXIDE_PROGRAM_FIXTURES";

fn recordings() -> Vec<PathBuf> {
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
    directories
}

fn read_lines(directory: &Path, file: &str) -> Vec<String> {
    std::fs::read_to_string(directory.join(file))
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Each recorded download again, this time with the transport layer in between: the
/// simulated device answers from the recording, and what it takes, frame by frame, must be
/// the recorded requests, line for line.
#[tokio::test(flavor = "multi_thread")]
async fn recorded_downloads_replay_through_the_transport() {
    for directory in recordings() {
        let name = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let read = |file: &str| std::fs::read_to_string(directory.join(file)).unwrap();
        let program = ApplicationProgram::parse(&read("app.xml")).unwrap();
        let device = DeviceData::parse(&read("device.xml")).unwrap();
        let addresses = GroupAddresses::parse_list(&read("group-addresses.txt")).unwrap();
        let mask = read_lines(&directory, "meta.txt")
            .iter()
            .find_map(|line| line.strip_prefix("mask=").map(str::to_string))
            .and_then(|hex| u16::from_str_radix(&hex, 16).ok())
            .unwrap();
        let recorded = read_lines(&directory, "requests.txt");
        let requests: Vec<Request> = recorded.iter().map(|line| line.parse().unwrap()).collect();
        let responses: Vec<Response> = read_lines(&directory, "responses.txt")
            .iter()
            .map(|line| line.parse().unwrap())
            .collect();

        let download = Arc::new(Download::prepare(&program, &device, &addresses).unwrap());
        let simulated = SimulatedDevice::from_recording(mask, &requests, &responses).unwrap();
        let mut run = Run::with(simulated);
        // As the recorded tool: no check of its own added.
        let plan = run
            .download(&download, PlanOptions::default())
            .await
            .unwrap_or_else(|failure| panic!("{name}: {}", failure.message));
        let received = run.received();
        println!(
            "{name}: {:?}, recorded {} requests, device took {}",
            plan.kind,
            recorded.len(),
            received.len()
        );
        assert_eq!(received, recorded, "{name}");
        let modelled = run
            .bus
            .with(DEVICE, |endpoint| endpoint.device().modelled())
            .unwrap();
        assert_eq!(modelled, 0, "{name}: an answer had to be invented");
        assert!(run.notes().is_empty(), "{name}: {:?}", run.notes());
        let expected_kind = if name.contains("0705") {
            DownloadKind::PARTIAL
        } else {
            DownloadKind::FULL
        };
        assert_eq!(plan.kind, expected_kind, "{name}");
    }
}
