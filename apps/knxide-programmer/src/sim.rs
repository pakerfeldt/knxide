//! Simulated devices on a simulated bus: for tests, and for trying the page without touching
//! a real installation (`--simulate`).
//!
//! A [`SimEndpoint`] is the device's end of the transport layer in front of a
//! [`SimulatedDevice`]; it can lose frames on request. A [`SimBus`] holds endpoints by
//! address, and a [`SimLink`] is the tool's access to it.
//!
//! The simulated device keeps to the transport layer as a real one does where it matters for
//! a tool on a bad link: it sends one answer at a time, repeats an answer that is not
//! acknowledged (three times, then it closes the connection), acknowledges a repeated request
//! again without taking it twice, and meets data on a connection it does not have with a
//! disconnect.
//!
//! One other kind of device can be had with [`Faults::keeps_answers_to_itself`]: it does not
//! repeat an answer that is not acknowledged and closes the connection once nothing has
//! reached it for the idle time. That is how one device of mask 0021 looked from the
//! receiving side of a lossy link; see [`knxide_program::transport`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use knxide_core::{Apdu, Destination, IndividualAddress, LData, Priority, Tpdu};
use knxide_program::{Request, SimulatedDevice, wire};
use tokio::time::Instant;

use crate::link::{Link, LinkError};

/// Frames a simulated device loses, counted down as they are lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Faults {
    /// Numbered frames from the tool that never reach the device.
    pub lose_requests: u32,
    /// Acknowledgements of the device that never reach the tool. The device has taken the
    /// request and answers it.
    pub lose_acks: u32,
    /// Answers of the device that never reach the tool. The device does not repeat them.
    pub lose_responses: u32,
    /// The device drops its connection, without a word, after taking this many more numbered
    /// frames, as a device does whose connection timed out. The next numbered frame is then
    /// answered with a disconnect.
    pub forget_connection_after: Option<u32>,
    /// The device takes this many more numbered frames and then nothing at all, as if it had
    /// been unplugged.
    pub silent_after: Option<u32>,
    /// Of the requests that have an answer, every `n`th loses both the device's
    /// acknowledgement and its answer on the way to the tool: the two travel back to back, so
    /// one gap in reception takes both. The device has taken the request and believes its
    /// answer sent.
    pub lose_exchange_every: Option<u32>,
    /// The device keeps an answer that is not acknowledged to itself: it does not send it
    /// again, and closes the connection when nothing has reached it for the bus's idle time
    /// (six seconds unless set otherwise). A repeated request is acknowledged again and not
    /// answered again, as always.
    pub keeps_answers_to_itself: bool,
    /// The next exchanges that have an answer lose acknowledgement and answer, like every
    /// `n`th with [`Faults::lose_exchange_every`], this many times.
    pub lose_exchanges: u32,
    /// The next answers are lost on the way to the tool, this many: the device has sent them
    /// and waits for their acknowledgement. The acknowledgement of the request arrives.
    pub lose_answers_on_the_way: u32,
    /// The device acknowledges the next writes, this many, and neither carries them out nor
    /// answers them.
    pub ignore_writes: u32,
    /// Echoing of memory writes (bit 2 of the device control property) ends with the
    /// connection, as a setting that belongs to a connection does.
    pub verify_mode_ends_with_connection: bool,
}

/// The device's end of a transport connection, in front of a simulated device.
#[derive(Debug)]
pub struct SimEndpoint {
    address: IndividualAddress,
    device: SimulatedDevice,
    /// The tool the connection is with.
    peer: Option<IndividualAddress>,
    /// Who was connected last; a disconnect from there is noted even after a restart.
    last_peer: Option<IndividualAddress>,
    receive_sequence: u8,
    send_sequence: u8,
    /// The answer on its way: sent, not yet acknowledged, with the repetitions it has left.
    in_flight: Option<(Tpdu, u8)>,
    /// Answers that wait for the one before them to be acknowledged.
    waiting: VecDeque<Apdu>,
    received: Vec<Request>,
    /// Requests with an answer taken so far, for [`Faults::lose_exchange_every`].
    exchanges: u32,
    /// Faults to inject.
    pub faults: Faults,
}

/// How often a device repeats an answer that is not acknowledged.
const ANSWER_REPETITIONS: u8 = 3;

impl SimEndpoint {
    /// `device` at `address`, not connected.
    pub fn new(address: IndividualAddress, device: SimulatedDevice) -> Self {
        Self {
            address,
            device,
            peer: None,
            last_peer: None,
            receive_sequence: 0,
            send_sequence: 0,
            in_flight: None,
            waiting: VecDeque::new(),
            received: Vec::new(),
            exchanges: 0,
            faults: Faults::default(),
        }
    }

    /// The tool an answer is on its way to and has not been acknowledged by.
    pub fn awaits_acknowledgement(&self) -> Option<IndividualAddress> {
        self.in_flight.as_ref().and(self.peer)
    }

    /// Whether the device waits out the idle time with an unacknowledged answer instead of
    /// repeating it.
    pub fn keeps_answers_to_itself(&self) -> bool {
        self.faults.keeps_answers_to_itself
    }

    /// The device's patience with an unacknowledged answer ran out: the answer on its way is
    /// sent again, or, after the last repetition, the connection is closed. A device that
    /// keeps its answers to itself closes the connection at once.
    pub fn repeat(&mut self) -> Option<LData> {
        let peer = self.peer?;
        let quiet = self.faults.keeps_answers_to_itself;
        let (frame, left) = self.in_flight.as_mut()?;
        if *left == 0 || quiet {
            self.drop_connection();
            return Some(self.frame(peer, Tpdu::Disconnect));
        }
        *left -= 1;
        let frame = frame.clone();
        Some(self.frame(peer, frame))
    }

    fn drop_connection(&mut self) {
        if self.faults.verify_mode_ends_with_connection && self.peer.is_some() {
            self.device.set_property(0, 14, 1, &[0]);
        }
        self.peer = None;
        self.in_flight = None;
        self.waiting.clear();
    }

    /// Send `apdu` as the next numbered frame, or queue it behind the answer on its way.
    fn answer(&mut self, tool: IndividualAddress, apdu: Apdu, out: &mut Vec<LData>) {
        if self.in_flight.is_some() {
            self.waiting.push_back(apdu);
            return;
        }
        let frame = Tpdu::Connected {
            sequence: self.send_sequence,
            apdu,
        };
        self.in_flight = Some((frame.clone(), ANSWER_REPETITIONS));
        out.push(self.frame(tool, frame));
    }

    /// The device's individual address.
    pub fn address(&self) -> IndividualAddress {
        self.address
    }

    /// The simulated device.
    pub fn device(&self) -> &SimulatedDevice {
        &self.device
    }

    /// The simulated device, to change what it holds.
    pub fn device_mut(&mut self) -> &mut SimulatedDevice {
        &mut self.device
    }

    /// Every request the device has taken, connects and disconnects included, in order. A
    /// repeated frame counts once.
    pub fn received(&self) -> &[Request] {
        &self.received
    }

    /// Forget the requests taken so far.
    pub fn clear_received(&mut self) {
        self.received.clear();
    }

    fn frame(&self, to: IndividualAddress, tpdu: Tpdu) -> LData {
        let mut frame = LData::new(Destination::Individual(to), tpdu);
        frame.source = self.address;
        frame.priority = Priority::System;
        frame
    }

    /// Take one frame from the bus and return the frames the device sends in answer.
    pub fn handle(&mut self, frame: &LData) -> Vec<LData> {
        if frame.destination != Destination::Individual(self.address) {
            return Vec::new();
        }
        let tool = frame.source;
        let mut out = Vec::new();
        match (&mut self.faults.silent_after, &frame.tpdu) {
            (Some(0), _) => return out,
            (Some(left), Tpdu::Connected { .. }) => *left -= 1,
            _ => {}
        }
        match &frame.tpdu {
            Tpdu::Connect => {
                self.drop_connection();
                self.peer = Some(tool);
                self.last_peer = Some(tool);
                self.receive_sequence = 0;
                self.send_sequence = 0;
                self.received.push(Request::Connect);
            }
            Tpdu::Disconnect => {
                if self.last_peer == Some(tool) {
                    self.received.push(Request::Disconnect);
                    self.last_peer = None;
                }
                if self.peer == Some(tool) {
                    self.drop_connection();
                }
            }
            Tpdu::Connected { sequence, apdu } => {
                if self.faults.lose_requests > 0 {
                    self.faults.lose_requests -= 1;
                    return out;
                }
                match &mut self.faults.forget_connection_after {
                    Some(0) => {
                        self.faults.forget_connection_after = None;
                        self.drop_connection();
                    }
                    Some(left) => *left -= 1,
                    None => {}
                }
                if self.peer != Some(tool) {
                    // Data without a connection is answered with a disconnect.
                    out.push(self.frame(tool, Tpdu::Disconnect));
                    return out;
                }
                let acknowledge = Tpdu::Ack {
                    sequence: *sequence,
                };
                if *sequence == self.receive_sequence.wrapping_sub(1) & 0x0F {
                    // A repetition: acknowledged again, not taken again.
                    out.push(self.frame(tool, acknowledge));
                    return out;
                }
                if *sequence != self.receive_sequence {
                    return out;
                }
                self.receive_sequence = (self.receive_sequence + 1) & 0x0F;
                let request = wire::request_from_apdu(apdu);
                if self.faults.ignore_writes > 0
                    && matches!(
                        request,
                        Some(Request::MemoryWrite { .. } | Request::PropertyWrite { .. })
                    )
                {
                    self.faults.ignore_writes -= 1;
                    out.push(self.frame(tool, acknowledge));
                    return out;
                }
                let answer = request
                    .as_ref()
                    .and_then(|request| self.device.answer(request))
                    .and_then(|answer| wire::response_apdu(&answer).ok());
                // Acknowledgement and answer lost together: nothing of this exchange reaches
                // the tool, and the device is none the wiser.
                let periodic = answer.is_some()
                    && self.faults.lose_exchange_every.is_some_and(|every| {
                        self.exchanges += 1;
                        every > 0 && self.exchanges.is_multiple_of(every)
                    });
                let counted = answer.is_some() && !periodic && self.faults.lose_exchanges > 0;
                if counted {
                    self.faults.lose_exchanges -= 1;
                }
                let exchange_lost = periodic || counted;
                if exchange_lost {
                } else if self.faults.lose_acks > 0 {
                    self.faults.lose_acks -= 1;
                } else {
                    out.push(self.frame(tool, acknowledge));
                }
                let Some(request) = request else {
                    return out;
                };
                let restart = request == Request::Restart;
                self.received.push(request);
                if restart {
                    // The device starts anew; the connection is gone.
                    self.drop_connection();
                }
                if let Some(apdu) = answer {
                    if exchange_lost {
                        self.answer(tool, apdu, &mut Vec::new());
                    } else if self.faults.lose_answers_on_the_way > 0 {
                        self.faults.lose_answers_on_the_way -= 1;
                        self.answer(tool, apdu, &mut Vec::new());
                    } else if self.faults.lose_responses > 0 {
                        self.faults.lose_responses -= 1;
                    } else {
                        self.answer(tool, apdu, &mut out);
                    }
                }
            }
            Tpdu::Ack { sequence } => {
                if self.peer == Some(tool)
                    && *sequence == self.send_sequence
                    && self.in_flight.take().is_some()
                {
                    self.send_sequence = (self.send_sequence + 1) & 0x0F;
                    if let Some(next) = self.waiting.pop_front() {
                        self.answer(tool, next, &mut out);
                    }
                }
            }
            Tpdu::Nak { .. } | Tpdu::Data(_) | Tpdu::TagGroup(_) => {}
        }
        out
    }
}

/// Simulated devices by individual address.
#[derive(Debug, Clone)]
pub struct SimBus {
    endpoints: Arc<Mutex<HashMap<IndividualAddress, SimEndpoint>>>,
    /// How long a device waits for the acknowledgement of an answer before it repeats it.
    answer_timeout: Duration,
    /// How long a device keeps a connection on which nothing reaches it.
    idle_timeout: Duration,
    /// How long a frame takes to get to a device.
    frame_delay: Duration,
}

impl Default for SimBus {
    fn default() -> Self {
        Self {
            endpoints: Arc::default(),
            // Spec 3/3/4: the acknowledgement timeout of a transport connection.
            answer_timeout: Duration::from_secs(3),
            // Spec 3/3/4: the connection timeout.
            idle_timeout: Duration::from_secs(6),
            frame_delay: Duration::ZERO,
        }
    }
}

impl SimBus {
    /// A bus without devices.
    pub fn new() -> Self {
        Self::default()
    }

    /// A bus without devices, on which a device repeats an unacknowledged answer after
    /// `answer_timeout` instead of the standard three seconds.
    pub fn with_answer_timeout(answer_timeout: Duration) -> Self {
        Self {
            answer_timeout,
            ..Self::default()
        }
    }

    /// The same bus with another idle time: how long a device keeps a connection on which
    /// nothing reaches it. It matters for devices that keep their answers to themselves.
    #[must_use]
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }

    /// The same bus with frames that take `frame_delay` to reach a device, so that an
    /// operation takes about as long as on a real bus and can be watched.
    #[must_use]
    pub fn with_frame_delay(mut self, frame_delay: Duration) -> Self {
        self.frame_delay = frame_delay;
        self
    }

    /// How long until a device with an unacknowledged answer on its way to `tool` does
    /// something about it, counted from the last frame that reached it; `None` when no
    /// device waits.
    fn patience(&self, tool: IndividualAddress) -> Option<Duration> {
        self.lock()
            .values()
            .filter(|endpoint| endpoint.awaits_acknowledgement() == Some(tool))
            .map(|endpoint| {
                if endpoint.keeps_answers_to_itself() {
                    self.idle_timeout
                } else {
                    self.answer_timeout
                }
            })
            .min()
    }

    /// The acknowledgement timers of the devices ran out: collect what they send to `tool`.
    fn repeat(&self, tool: IndividualAddress) -> Vec<LData> {
        self.lock()
            .values_mut()
            .filter(|endpoint| endpoint.awaits_acknowledgement() == Some(tool))
            .filter_map(SimEndpoint::repeat)
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<IndividualAddress, SimEndpoint>> {
        self.endpoints
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Put `device` on the bus at `address`, replacing what was there.
    pub fn add(&self, address: IndividualAddress, device: SimulatedDevice) {
        self.lock()
            .insert(address, SimEndpoint::new(address, device));
    }

    /// Look at or change the device at `address`.
    pub fn with<T>(
        &self,
        address: IndividualAddress,
        work: impl FnOnce(&mut SimEndpoint) -> T,
    ) -> Option<T> {
        self.lock().get_mut(&address).map(work)
    }

    /// Put a frame on the bus and collect the answers.
    pub fn deliver(&self, frame: &LData) -> Vec<LData> {
        let Destination::Individual(address) = frame.destination else {
            return Vec::new();
        };
        self.lock()
            .get_mut(&address)
            .map(|endpoint| endpoint.handle(frame))
            .unwrap_or_default()
    }

    /// Whether a device sits at `address`.
    pub fn has(&self, address: IndividualAddress) -> bool {
        self.lock().contains_key(&address)
    }

    /// The tool's access to this bus, with `address` as the tool's own.
    pub fn link(&self, address: IndividualAddress) -> SimLink {
        SimLink {
            bus: self.clone(),
            address,
            inbox: VecDeque::new(),
            repeat_due: None,
        }
    }
}

/// A [`Link`] to a [`SimBus`]. Like an interface on a real bus it reports a frame to an
/// address without device as not acknowledged.
#[derive(Debug)]
pub struct SimLink {
    bus: SimBus,
    address: IndividualAddress,
    inbox: VecDeque<(IndividualAddress, Tpdu)>,
    /// When the devices' acknowledgement timers run out, while an answer is on its way.
    repeat_due: Option<Instant>,
}

impl Link for SimLink {
    fn address(&self) -> IndividualAddress {
        self.address
    }

    async fn send(&mut self, device: IndividualAddress, tpdu: Tpdu) -> Result<(), LinkError> {
        if !self.bus.has(device) {
            return Err(LinkError::NotAcknowledged);
        }
        if !self.bus.frame_delay.is_zero() {
            tokio::time::sleep(self.bus.frame_delay).await;
        }
        let mut frame = LData::new(Destination::Individual(device), tpdu);
        frame.source = self.address;
        frame.priority = Priority::System;
        for answer in self.bus.deliver(&frame) {
            if answer.destination == Destination::Individual(self.address) {
                self.inbox.push_back((answer.source, answer.tpdu));
            }
        }
        // Whatever is on its way now has a full timeout to be acknowledged in.
        self.repeat_due = None;
        Ok(())
    }

    async fn receive(&mut self) -> Option<(IndividualAddress, Tpdu)> {
        loop {
            if let Some(frame) = self.inbox.pop_front() {
                return Some(frame);
            }
            let Some(patience) = self.bus.patience(self.address) else {
                // Nothing arrives unless something is sent.
                self.repeat_due = None;
                return std::future::pending().await;
            };
            // A device waits for an acknowledgement: when its patience runs out it repeats
            // its answer, or closes the connection.
            let due = *self
                .repeat_due
                .get_or_insert_with(|| Instant::now() + patience);
            tokio::time::sleep_until(due).await;
            self.repeat_due = None;
            for frame in self.bus.repeat(self.address) {
                if frame.destination == Destination::Individual(self.address) {
                    self.inbox.push_back((frame.source, frame.tpdu));
                }
            }
        }
    }

    async fn close(self) {}
}
