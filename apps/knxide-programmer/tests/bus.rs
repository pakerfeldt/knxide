//! The same operations over KNXnet/IP on the loopback interface: a tunnel to the test kit's
//! fake gateway, and routing to a stand-in router reached by unicast. The device behind both
//! is simulated; no bus and no multicast is involved.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use knxide_core::{Cemi, IndividualAddress, LData};
use knxide_ip::Frame;
use knxide_program::{Download, PlanOptions, Project, ProjectArchive, Request, SimulatedDevice};
use knxide_programmer::interfaces::Kind;
use knxide_programmer::link::{BusAccess, ClientLink, Link};
use knxide_programmer::ops::{self, Cancel, CompareLimits, DownloadLimits};
use knxide_programmer::session::{Session, SessionConfig};
use knxide_programmer::sim::SimEndpoint;
use knxide_testkit::FakeGateway;

const DEVICE: IndividualAddress = IndividualAddress::from_raw(0x1109);

fn download_for(mask: u16) -> Arc<Download> {
    let bytes = knxide_testkit::project::archive();
    let project = Project::load(&ProjectArchive::open(&bytes).unwrap()).unwrap();
    project
        .devices
        .iter()
        .filter_map(|device| device.download.clone())
        .find(|download| download.profile.mask_version == mask)
        .map(Arc::new)
        .unwrap()
}

fn options() -> PlanOptions {
    PlanOptions {
        require_same_application: true,
        ..PlanOptions::default()
    }
}

fn lines(requests: &[Request]) -> Vec<String> {
    requests.iter().map(ToString::to_string).collect()
}

fn received(endpoint: &Mutex<SimEndpoint>) -> Vec<String> {
    lines(endpoint.lock().unwrap().received())
}

#[tokio::test(flavor = "multi_thread")]
async fn download_over_a_tunnel() {
    let download = download_for(0x0021);
    let gateway = FakeGateway::spawn().unwrap();
    let endpoint = Arc::new(Mutex::new(SimEndpoint::new(
        DEVICE,
        SimulatedDevice::holding(&download),
    )));
    let device = Arc::clone(&endpoint);
    gateway.attach_device(
        DEVICE,
        Box::new(move |frame: &LData| device.lock().unwrap().handle(frame)),
    );

    let access = BusAccess::Tunnel {
        gateway: gateway.address(),
        tcp: false,
    };
    let link = ClientLink::open(&access).await.unwrap();
    // The tool's address is the one the gateway gave the tunnel.
    assert_eq!(link.address().to_string(), "1.1.251");
    let mut session = Session::new(link, DEVICE, SessionConfig::default());
    let plan = ops::download(
        &mut session,
        Arc::clone(&download),
        options(),
        DownloadLimits::none(),
        &mut |_| {},
        &mut Cancel::never(),
    )
    .await
    .unwrap()
    .plan;
    session.finish().await.close().await;

    assert_eq!(received(&endpoint), lines(&plan.requests));
    assert_eq!(plan.requests.last(), Some(&Request::Disconnect));
    // The tunnel is closed again: the interface is free for other tools.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(gateway.tunnel_count(), 0);

    // A comparison afterwards finds the device as planned.
    let link = ClientLink::open(&access).await.unwrap();
    let mut session = Session::new(link, DEVICE, SessionConfig::default());
    let comparison = ops::compare(
        &mut session,
        Arc::clone(&download),
        options(),
        CompareLimits::default(),
        &mut |_| {},
        &mut Cancel::never(),
    )
    .await
    .unwrap();
    session.finish().await.close().await;
    assert!(comparison.is_complete());
    assert_eq!((comparison.repetitions, comparison.reconnects), (0, 0));
    assert_eq!(comparison.differing, 0);
    assert!(comparison.compared > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_at_the_address_over_a_tunnel() {
    let download = download_for(0x0021);
    let gateway = FakeGateway::spawn().unwrap();
    let access = BusAccess::Tunnel {
        gateway: gateway.address(),
        tcp: false,
    };
    let link = ClientLink::open(&access).await.unwrap();
    let mut session = Session::new(link, DEVICE, SessionConfig::default());
    let failure = ops::download(
        &mut session,
        download,
        options(),
        DownloadLimits::none(),
        &mut |_| {},
        &mut Cancel::never(),
    )
    .await
    .unwrap_err();
    session.finish().await.close().await;
    // The interface reports that nobody acknowledged the connect: no waiting for timeouts.
    assert_eq!(failure.message, "no device answers at 1.1.9");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_gateway() {
    // A port nothing listens on.
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let gateway = socket.local_addr().unwrap();
    drop(socket);
    let mut access = BusAccess::Tunnel { gateway, tcp: true };
    let error = ClientLink::open(&access).await.err().unwrap();
    assert!(
        error.starts_with("cannot open a tunnel to 127.0.0.1"),
        "{error}"
    );
    if let BusAccess::Tunnel { tcp, .. } = &mut access {
        *tcp = false;
    }
    assert_eq!(access.to_string(), format!("tunnel to {gateway}"));
}

/// A router that is one simulated device: every routing indication goes to the device, and
/// its answers go back to the sender.
struct FakeRouter {
    address: SocketAddr,
    endpoint: Arc<Mutex<SimEndpoint>>,
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeRouter {
    fn spawn(endpoint: SimEndpoint) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(5)))
            .unwrap();
        let address = socket.local_addr().unwrap();
        let endpoint = Arc::new(Mutex::new(endpoint));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (endpoint, frames, stop) = (
                Arc::clone(&endpoint),
                Arc::clone(&frames),
                Arc::clone(&stop),
            );
            std::thread::spawn(move || {
                let mut buffer = [0u8; 512];
                while !stop.load(Ordering::Relaxed) {
                    let Ok((length, source)) = socket.recv_from(&mut buffer) else {
                        continue;
                    };
                    let Ok(Frame::RoutingIndication { cemi }) = Frame::decode(&buffer[..length])
                    else {
                        continue;
                    };
                    frames.lock().unwrap().push(cemi.clone());
                    let Ok(Cemi::LDataInd(frame)) = Cemi::decode(&cemi) else {
                        continue;
                    };
                    let answers = endpoint.lock().unwrap().handle(&frame);
                    for answer in answers {
                        let cemi = Cemi::LDataInd(answer).to_vec().unwrap();
                        let bytes = Frame::RoutingIndication { cemi }.to_vec().unwrap();
                        socket.send_to(&bytes, source).unwrap();
                    }
                }
            })
        };
        Self {
            address,
            endpoint,
            frames,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for FakeRouter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn download_over_routing() {
    let download = download_for(0x0705);
    let router = FakeRouter::spawn(SimEndpoint::new(
        DEVICE,
        SimulatedDevice::holding(&download),
    ));
    let tool: IndividualAddress = "0.0.2".parse().unwrap();
    let access = BusAccess::Routing {
        interface: Ipv4Addr::LOCALHOST,
        name: String::new(),
        kind: Kind::Unknown,
        address: tool,
        group: Some(router.address),
    };
    assert_eq!(
        access.to_string(),
        "routing on 127.0.0.1 (kind unknown) as 0.0.2"
    );
    assert!(!access.is_wireless());
    let named = BusAccess::Routing {
        interface: Ipv4Addr::LOCALHOST,
        name: "wlan0".into(),
        kind: Kind::Wireless,
        address: tool,
        group: None,
    };
    assert_eq!(named.to_string(), "routing on wlan0 (wireless) as 0.0.2");
    assert!(named.is_wireless());
    let link = ClientLink::open(&access).await.unwrap();
    assert_eq!(link.address(), tool);
    let mut session = Session::new(link, DEVICE, SessionConfig::default());
    let plan = ops::download(
        &mut session,
        Arc::clone(&download),
        options(),
        DownloadLimits::none(),
        &mut |_| {},
        &mut Cancel::never(),
    )
    .await
    .unwrap()
    .plan;
    session.finish().await.close().await;
    // Routing has no confirmation: the last frame may still be on its way to the router.
    for _ in 0..200 {
        if received(&router.endpoint).len() >= plan.requests.len() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(received(&router.endpoint), lines(&plan.requests));

    // On the wire: cEMI L_Data.ind, control octets B0 60, from the tool's address to the
    // device's. The first frame is the connect, the second the descriptor read with sequence
    // number 0.
    let frames = router.frames.lock().unwrap();
    assert_eq!(
        frames[0],
        [0x29, 0x00, 0xB0, 0x60, 0x00, 0x02, 0x11, 0x09, 0x00, 0x80]
    );
    assert_eq!(
        frames[1],
        [
            0x29, 0x00, 0xB0, 0x60, 0x00, 0x02, 0x11, 0x09, 0x01, 0x43, 0x00
        ]
    );
    for frame in frames.iter() {
        assert_eq!(frame[..8], [0x29, 0x00, 0xB0, 0x60, 0x00, 0x02, 0x11, 0x09]);
        // Nothing longer than a standard frame: at most 15 octets after the TPCI octet.
        assert!(frame[8] <= 15, "{frame:02X?}");
    }
}
