//! Sans-IO gateway discovery: `SEARCH_REQUEST` to the multicast group, or
//! `DESCRIPTION_REQUEST` to one known host. Spec: 3/8/2 (Core) §4.
//!
//! Same pattern as [`crate::tunnel`]: drain [`Discovery::poll_transmit`], feed responses to
//! [`Discovery::handle_input`], and stop at [`Discovery::poll_timeout`].

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use crate::frame::{Frame, KNXNETIP_PORT};
use crate::structures::{DeviceInfo, Dib, Dibs, Hpai, ServiceFamily};
use crate::tunnel::Transmit;

/// KNXnet/IP system setup multicast address. Spec: 3/8/2.
pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 23, 12);

/// Where search requests go: `224.0.23.12:3671`.
pub const MULTICAST_ENDPOINT: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(MULTICAST_GROUP, KNXNETIP_PORT));

/// A KNXnet/IP server that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gateway {
    /// Where to send connect requests.
    pub control_endpoint: SocketAddr,
    /// Everything the server described about itself.
    pub dibs: Vec<Dib>,
}

impl Gateway {
    /// Device information, if the server sent it (it should).
    pub fn device_info(&self) -> Option<&DeviceInfo> {
        self.dibs.device_info()
    }

    /// Friendly name, or an empty string.
    pub fn name(&self) -> &str {
        self.device_info().map_or("", |d| d.name.as_str())
    }

    /// Supported service families.
    pub fn service_families(&self) -> &[ServiceFamily] {
        self.dibs.service_families()
    }

    /// Whether the server offers tunnelling.
    pub fn supports_tunnelling(&self) -> bool {
        self.dibs.supports(ServiceFamily::TUNNELLING)
    }

    /// Whether the server routes (is a KNX IP router).
    pub fn supports_routing(&self) -> bool {
        self.dibs.supports(ServiceFamily::ROUTING)
    }

    /// Whether the server offers KNX IP Secure.
    pub fn supports_security(&self) -> bool {
        self.dibs.supports(ServiceFamily::SECURITY)
    }
}

/// A discovery run.
#[derive(Debug)]
pub struct Discovery {
    deadline: Instant,
    transmits: VecDeque<Transmit>,
    found: Vec<Gateway>,
    new: VecDeque<Gateway>,
}

impl Discovery {
    /// Search for servers by multicast. `local` is where responses should go: our socket's
    /// address, or [`Hpai::ROUTE_BACK`]. Not every server honours route-back for searches,
    /// so prefer the real address when it is known.
    pub fn search(local: Hpai, timeout: Duration, now: Instant) -> Self {
        Self::with_request(
            MULTICAST_ENDPOINT,
            &Frame::SearchRequest {
                discovery_endpoint: local,
            },
            timeout,
            now,
        )
    }

    /// Ask one server to describe itself, for networks where multicast does not reach it.
    pub fn describe(target: SocketAddr, local: Hpai, timeout: Duration, now: Instant) -> Self {
        Self::with_request(
            target,
            &Frame::DescriptionRequest {
                control_endpoint: local,
            },
            timeout,
            now,
        )
    }

    fn with_request(
        destination: SocketAddr,
        frame: &Frame,
        timeout: Duration,
        now: Instant,
    ) -> Self {
        let mut transmits = VecDeque::new();
        if let Ok(payload) = frame.to_vec() {
            transmits.push_back(Transmit {
                destination,
                payload,
            });
        }
        Self {
            deadline: now + timeout,
            transmits,
            found: Vec::new(),
            new: VecDeque::new(),
        }
    }

    /// Next datagram to send.
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.transmits.pop_front()
    }

    /// When discovery ends.
    pub fn poll_timeout(&self) -> Instant {
        self.deadline
    }

    /// Whether the time is up.
    pub fn is_done(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Process a datagram. Responses arriving after the deadline are ignored.
    pub fn handle_input(&mut self, source: SocketAddr, datagram: &[u8], now: Instant) {
        if self.is_done(now) {
            return;
        }
        let gateway = match Frame::decode(datagram) {
            Ok(Frame::SearchResponse {
                control_endpoint,
                dibs,
            }) => Gateway {
                control_endpoint: control_endpoint.resolve(source),
                dibs,
            },
            Ok(Frame::DescriptionResponse { dibs }) => Gateway {
                control_endpoint: source,
                dibs,
            },
            _ => return,
        };
        // Servers answer once per interface and sometimes repeat; keep the first answer.
        if self
            .found
            .iter()
            .any(|g| g.control_endpoint == gateway.control_endpoint)
        {
            return;
        }
        self.found.push(gateway.clone());
        self.new.push_back(gateway);
    }

    /// Next newly found gateway.
    pub fn poll_gateway(&mut self) -> Option<Gateway> {
        self.new.pop_front()
    }

    /// Every gateway found so far.
    pub fn gateways(&self) -> &[Gateway] {
        &self.found
    }
}
