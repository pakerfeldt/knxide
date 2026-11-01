//! Sans-IO KNXnet/IP routing client. Spec: 3/8/5 (Routing).
//!
//! Routing is connectionless: `ROUTING_INDICATION` frames carrying `L_Data.ind` are multicast to
//! `224.0.23.12:3671`. Only KNX IP routers take part; plain IP interfaces do not route.
//!
//! Flow control follows 3/8/5 §2.3.5: at most one indication every 20 ms (about 50 per
//! second), and on `ROUTING_BUSY` a pause for the requested wait time plus a random extension
//! that grows with the number of busy frames seen recently. The constants match xknx.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use knxide_core::{Cemi, IndividualAddress};

use crate::discovery::MULTICAST_ENDPOINT;
use crate::frame::Frame;
use crate::tunnel::{SendError, SendId, Transmit};

/// Minimum spacing between our indications: 20 ms. Spec 3/8/5 §2.3.5.
pub const MIN_INTERVAL: Duration = Duration::from_millis(20);
/// Busy frames closer together than this count once: 10 ms.
const BUSY_INCREMENT_COOLDOWN: Duration = Duration::from_millis(10);
/// Random pause extension per counted busy frame: up to 50 ms.
const BUSY_RANDOM_FACTOR_MS: f64 = 50.0;
/// Slow-down period per counted busy frame after the pause: 100 ms.
const BUSY_SLOWDURATION: Duration = Duration::from_millis(100);
/// After the slow-down, the busy count decays by one every 5 ms.
const BUSY_DECREMENT: Duration = Duration::from_millis(5);

/// Router settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterConfig {
    /// Where indications go. Default `224.0.23.12:3671`.
    pub group: SocketAddr,
    /// Source address for frames we send. A routing client has no tunnel to assign one, so
    /// pick a free address on the IP backbone/line. Default `15.15.250`.
    pub individual_address: IndividualAddress,
    /// Ignore datagrams from this address: our own frames looped back by the network stack.
    pub own_endpoint: Option<SocketAddr>,
    /// Minimum spacing between indications. Default [`MIN_INTERVAL`].
    pub min_interval: Duration,
    /// Most frames that may wait to be sent.
    pub max_queue_len: usize,
    /// Seed for the random pause extension.
    pub seed: u64,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            group: MULTICAST_ENDPOINT,
            individual_address: IndividualAddress::from_raw(0xFFFA),
            own_endpoint: None,
            min_interval: MIN_INTERVAL,
            max_queue_len: 256,
            seed: 0x6B6E_7800_C0FF_EE01,
        }
    }
}

/// Something that happened, drained with [`RouterClient::poll_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterEvent {
    /// A frame from the network (normally `L_Data.ind`).
    Received(Cemi),
    /// A queued frame went out. Routing has no confirmation, so this is all we learn.
    Sent(SendId),
    /// A router asked us to pause.
    Busy {
        /// Requested pause.
        wait: Duration,
    },
    /// A router reported dropping frames.
    LostMessages {
        /// Number lost.
        count: u16,
        /// Router's device state.
        device_state: u8,
    },
}

struct Queued {
    id: SendId,
    payload: Vec<u8>,
}

/// A KNXnet/IP routing client state machine.
pub struct RouterClient {
    config: RouterConfig,
    queue: VecDeque<Queued>,
    next_id: u64,
    last_sent: Option<Instant>,
    flow: FlowControl,
    transmits: VecDeque<Transmit>,
    events: VecDeque<RouterEvent>,
}

impl RouterClient {
    /// Create a client.
    pub fn new(config: RouterConfig) -> Self {
        let flow = FlowControl::new(config.seed);
        Self {
            config,
            queue: VecDeque::new(),
            next_id: 0,
            last_sent: None,
            flow,
            transmits: VecDeque::new(),
            events: VecDeque::new(),
        }
    }

    /// The configuration.
    pub fn config(&self) -> &RouterConfig {
        &self.config
    }

    /// Queue a frame. `L_Data.req` is sent as `L_Data.ind`, as routers expect, and a source of
    /// `0.0.0` is replaced with [`RouterConfig::individual_address`].
    pub fn send(&mut self, cemi: &Cemi, now: Instant) -> Result<SendId, SendError> {
        if self.queue.len() >= self.config.max_queue_len {
            return Err(SendError::QueueFull);
        }
        let mut ldata = match cemi {
            Cemi::LDataReq(l) | Cemi::LDataInd(l) => l.clone(),
            _ => {
                return Err(SendError::Encode(knxide_core::EncodeError::OutOfRange(
                    "routing needs L_Data",
                )));
            }
        };
        if ldata.source == IndividualAddress::UNSET {
            ldata.source = self.config.individual_address;
        }
        let cemi = Cemi::LDataInd(ldata).to_vec().map_err(SendError::Encode)?;
        let payload = Frame::RoutingIndication { cemi }
            .to_vec()
            .map_err(SendError::Encode)?;
        let id = SendId(self.next_id);
        self.next_id += 1;
        self.queue.push_back(Queued { id, payload });
        self.pump(now);
        Ok(id)
    }

    /// Next datagram to send.
    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.transmits.pop_front()
    }

    /// Next event.
    pub fn poll_event(&mut self) -> Option<RouterEvent> {
        self.events.pop_front()
    }

    /// When [`RouterClient::handle_timeout`] should next be called: only while frames wait.
    pub fn poll_timeout(&self) -> Option<Instant> {
        if self.queue.is_empty() {
            None
        } else {
            self.next_send_time()
        }
    }

    /// Whether sending is paused by `ROUTING_BUSY` at `now`.
    pub fn is_paused(&self, now: Instant) -> bool {
        self.flow.paused_until.is_some_and(|t| now < t)
    }

    /// Process a datagram. Our own frames (see [`RouterConfig::own_endpoint`]), undecodable
    /// datagrams and other services are ignored.
    pub fn handle_input(&mut self, source: SocketAddr, datagram: &[u8], now: Instant) {
        if self.config.own_endpoint == Some(source) {
            return;
        }
        match Frame::decode(datagram) {
            Ok(Frame::RoutingIndication { cemi }) => match Cemi::decode(&cemi) {
                Ok(cemi) => self.events.push_back(RouterEvent::Received(cemi)),
                Err(err) => tracing::debug!(%source, %err, "ignoring undecodable cEMI frame"),
            },
            Ok(Frame::RoutingBusy { wait_time_ms, .. }) => {
                let wait = Duration::from_millis(wait_time_ms.into());
                self.flow.on_busy(wait, now);
                self.events.push_back(RouterEvent::Busy { wait });
            }
            Ok(Frame::RoutingLostMessage {
                device_state,
                lost_messages,
            }) => {
                self.events.push_back(RouterEvent::LostMessages {
                    count: lost_messages,
                    device_state,
                });
            }
            Ok(_) => {}
            Err(err) => tracing::debug!(%source, %err, "ignoring undecodable datagram"),
        }
        self.pump(now);
    }

    /// Send whatever flow control allows at `now`.
    pub fn handle_timeout(&mut self, now: Instant) {
        self.pump(now);
    }

    /// Earliest time the next frame may go. `None` means now (nothing holds it back).
    fn next_send_time(&self) -> Option<Instant> {
        let spaced = self.last_sent.map(|t| t + self.config.min_interval);
        [spaced, self.flow.paused_until].into_iter().flatten().max()
    }

    fn pump(&mut self, now: Instant) {
        if self.queue.is_empty() {
            return;
        }
        let ready = self
            .last_sent
            .is_none_or(|t| now >= t + self.config.min_interval)
            && !self.is_paused(now);
        if !ready {
            return;
        }
        if let Some(next) = self.queue.pop_front() {
            self.last_sent = Some(now);
            self.transmits.push_back(Transmit {
                destination: self.config.group,
                payload: next.payload,
            });
            self.events.push_back(RouterEvent::Sent(next.id));
        }
    }
}

/// `ROUTING_BUSY` handling, 3/8/5 §2.3.5.
struct FlowControl {
    paused_until: Option<Instant>,
    wait_start: Option<Instant>,
    wait: Duration,
    busy_count: u32,
    last_busy: Option<Instant>,
    /// When the busy count starts decaying.
    decay_from: Option<Instant>,
    rng: u64,
}

impl FlowControl {
    fn new(seed: u64) -> Self {
        Self {
            paused_until: None,
            wait_start: None,
            wait: Duration::ZERO,
            busy_count: 0,
            last_busy: None,
            decay_from: None,
            rng: seed | 1,
        }
    }

    /// xorshift64*, uniform in [0, 1).
    fn random(&mut self) -> f64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// The busy count at `now`, after decay.
    fn decayed_count(&self, now: Instant) -> u32 {
        match self.decay_from {
            Some(from) if now > from => {
                let steps = (now - from).as_nanos() / BUSY_DECREMENT.as_nanos();
                self.busy_count
                    .saturating_sub(u32::try_from(steps).unwrap_or(u32::MAX))
            }
            _ => self.busy_count,
        }
    }

    fn on_busy(&mut self, wait: Duration, now: Instant) {
        self.busy_count = self.decayed_count(now);
        let previous = self.last_busy.replace(now);
        let pausing = self.paused_until.is_some_and(|t| now < t);
        if pausing {
            if previous.is_some_and(|p| now - p > BUSY_INCREMENT_COOLDOWN) {
                self.busy_count += 1;
            }
            // A shorter wait than what is left does not shorten the pause.
            if let Some(start) = self.wait_start
                && (start + self.wait).saturating_duration_since(now) >= wait
            {
                return;
            }
        }
        self.wait = wait;
        self.wait_start = Some(now);
        let extension_ms = self.random() * f64::from(self.busy_count) * BUSY_RANDOM_FACTOR_MS;
        let until = now + wait + Duration::from_secs_f64(extension_ms / 1000.0);
        self.paused_until = Some(until);
        self.decay_from = Some(until + BUSY_SLOWDURATION * self.busy_count);
    }
}
