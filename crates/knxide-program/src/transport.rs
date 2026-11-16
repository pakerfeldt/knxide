//! The tool's end of a transport layer connection to one device, as a state machine without
//! I/O: feed it the frames the device sends and the passing of time, and it says which frames
//! to send and what became of each request.
//!
//! ```
//! use std::time::Instant;
//! use knxide_core::Tpdu;
//! use knxide_program::transport::{Connection, TransportConfig, TransportEvent};
//! use knxide_program::{Request, wire};
//!
//! let now = Instant::now();
//! let mut connection = Connection::new(TransportConfig::default());
//! connection.open();
//! assert_eq!(connection.poll_transmit(), Some(Tpdu::Connect));
//!
//! let read = wire::request_apdu(&Request::DeviceDescriptorRead)?;
//! connection.request(read.clone(), true, now)?;
//! assert_eq!(
//!     connection.poll_transmit(),
//!     Some(Tpdu::Connected { sequence: 0, apdu: read })
//! );
//! // The device acknowledges, then answers; the answer is acknowledged in turn.
//! connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
//! let answer = wire::response_apdu(&"DEVICE_DESCRIPTOR_RESPONSE 0705".parse()?)?;
//! connection.handle_input(&Tpdu::Connected { sequence: 0, apdu: answer.clone() }, now);
//! assert_eq!(connection.poll_transmit(), Some(Tpdu::Ack { sequence: 0 }));
//! assert_eq!(connection.poll_event(), Some(TransportEvent::Response(answer)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Rules
//!
//! From the KNX standard, 3/3/4 (Transport Layer) §5:
//!
//! - `T_Connect` opens the connection and is not acknowledged. Both sides start counting at 0.
//! - Data travels in numbered frames (`T_Data_Connected`), each direction with its own 4-bit
//!   sequence number. The receiver answers every in-sequence frame with a `T_ACK` carrying the
//!   frame's number and then counts on; the sender counts on when that `T_ACK` arrives.
//! - A frame that is not acknowledged within 3 s is sent again, up to 3 repetitions. After that
//!   the connection is closed with `T_Disconnect`.
//! - A repeated frame (sequence number one behind) is acknowledged again and otherwise ignored.
//! - `T_NAK` for the pending frame makes the sender repeat it at once, within the same budget
//!   of repetitions.
//!
//! From behaviour notes on the tool whose downloads this crate reproduces (not seen on a bus):
//!
//! - An answer is awaited for 10 s from the acknowledgement of its request.
//! - A numbered frame with another sequence number is dropped while a request is pending; when
//!   nothing is pending it means the two ends have lost step, and the connection is closed.
//!
//! One point follows the standard where the notes differ: an in-sequence data frame that
//! arrives before the `T_ACK` of the pending request is acknowledged and kept as that request's
//! answer (the notes describe it as dropped, to be repeated by the device).
//!
//! Timeouts and repetitions are unverified: no recorded download contains a lost frame.
//!
//! # An answer that was lost together with its acknowledgement
//!
//! Off by default; see [`TransportConfig::lost_answer_timeout`]. It is meant for reads only,
//! where asking twice does no harm.
//!
//! Seen twice on one device of mask 0021, from the receiving side of a lossy link: the tool
//! repeats a request, the device acknowledges the repetition and sends no answer, and some six
//! seconds later it closes the connection. The reading of it, of medium confidence: the device
//! had taken the first transmission and answered it; its acknowledgement and its answer, sent
//! back to back, were both lost; the repetition is a duplicate to it, acknowledged and not
//! answered again; and it does not repeat its own unacknowledged answer.
//!
//! With the setting on, a request that was acknowledged only after a repetition waits that
//! long for its answer. Then the answer counts as lost: it is acknowledged unseen, with the
//! sequence number it must have had, so that both ends count on, and the request fails with
//! [`TransportFailure::AnswerLost`] on a connection that stays open. The caller sends the
//! request again as a new one. Should the answer arrive after all it is one behind by then,
//! and is acknowledged again and ignored like any repeated frame.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use knxide_core::{Apdu, Tpdu};

/// How long a numbered frame waits for its `T_ACK`. Spec: 3/3/4 §5, acknowledgement timeout.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(3);
/// How often an unacknowledged frame is sent again. Spec: 3/3/4 §5, `MAX_REP_COUNT`.
pub const MAX_REPETITIONS: u8 = 3;
/// How long an answer is awaited. From behaviour notes.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Sequence numbers are four bits wide.
const SEQUENCE_MASK: u8 = 0x0F;

/// Timers and limits of a [`Connection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportConfig {
    /// Wait for a `T_ACK` per transmission.
    pub ack_timeout: Duration,
    /// Repetitions of an unacknowledged frame; it is sent `repetitions + 1` times in all.
    pub repetitions: u8,
    /// Wait for the answer to a request, from its acknowledgement.
    pub response_timeout: Duration,
    /// After a request was acknowledged only on a repetition: how long to wait for its answer
    /// before taking it as lost, acknowledging it unseen and failing the request with
    /// [`TransportFailure::AnswerLost`]. `None`, the default, waits the whole
    /// `response_timeout` as for any request. For reads only; it rests on an inference of
    /// medium confidence, see the module documentation.
    pub lost_answer_timeout: Option<Duration>,
    /// After any acknowledgement of a request: how long to wait for its answer before taking
    /// it as lost in the same way. It takes the place of `response_timeout`; after a
    /// repetition `lost_answer_timeout` goes first where it is set. `None`, the default,
    /// leaves `response_timeout` and its plain "no answer".
    ///
    /// For a caller that then finds out by reading what the device made of the request, and
    /// does so before the device gives the connection up (about six seconds after the last
    /// frame it took). The value should leave a device that repeats its answer after three
    /// seconds the time to do so. This is the caller's own recovery, not behaviour of the
    /// tool whose downloads this crate reproduces.
    pub unanswered_timeout: Option<Duration>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            ack_timeout: ACK_TIMEOUT,
            repetitions: MAX_REPETITIONS,
            response_timeout: RESPONSE_TIMEOUT,
            lost_answer_timeout: None,
            unanswered_timeout: None,
        }
    }
}

/// Why a request did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailure {
    /// No `T_ACK` after every repetition. The connection has been closed.
    NotAcknowledged,
    /// The request was acknowledged but no answer came in time. The connection stays open.
    NoResponse,
    /// The device closed the connection.
    ClosedByDevice,
    /// The request was acknowledged on a repetition and its answer did not follow: the answer
    /// was taken as lost and acknowledged unseen. The connection stays open, and the request
    /// can be made again. Only with [`TransportConfig::lost_answer_timeout`].
    AnswerLost,
}

impl fmt::Display for TransportFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotAcknowledged => "the device did not acknowledge the request",
            Self::NoResponse => "the device did not answer in time",
            Self::ClosedByDevice => "the device closed the connection",
            Self::AnswerLost => "the answer was lost on the way",
        })
    }
}

impl std::error::Error for TransportFailure {}

/// Something that happened on the connection, drained with [`Connection::poll_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEvent {
    /// The pending request, which expects no answer, was acknowledged.
    Acknowledged,
    /// The answer to the pending request.
    Response(Apdu),
    /// The pending request failed.
    Failed(TransportFailure),
    /// A data frame arrived that no request was waiting for. It was acknowledged.
    Unsolicited(Apdu),
    /// The connection closed while no request was pending: the device closed it, or the two
    /// ends lost step.
    Closed,
}

/// A request was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    /// The connection is not open.
    NotConnected,
    /// Another request is still pending.
    Busy,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotConnected => "the connection is not open",
            Self::Busy => "another request is pending",
        })
    }
}

impl std::error::Error for RequestError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Closed,
    Idle,
    AwaitingAck {
        /// The frame as sent, for repetitions.
        frame: Tpdu,
        sequence: u8,
        /// How often the frame has been sent.
        transmissions: u8,
        deadline: Instant,
        expects_response: bool,
        /// An answer that overtook the acknowledgement.
        early_response: Option<Apdu>,
    },
    AwaitingResponse {
        deadline: Instant,
        /// Whether the answer counts as lost, not as missing, when the deadline passes.
        lost: bool,
    },
}

/// The tool's end of a transport connection. One request is pending at a time.
#[derive(Debug, Clone)]
pub struct Connection {
    config: TransportConfig,
    phase: Phase,
    /// Sequence number of the next numbered frame to send.
    send_sequence: u8,
    /// Sequence number expected in the next numbered frame from the device.
    receive_sequence: u8,
    transmits: VecDeque<Tpdu>,
    events: VecDeque<TransportEvent>,
    /// Repeated data frames from the device that were acknowledged again.
    duplicates: u32,
}

impl Connection {
    /// A closed connection.
    pub fn new(config: TransportConfig) -> Self {
        Self {
            config,
            phase: Phase::Closed,
            send_sequence: 0,
            receive_sequence: 0,
            transmits: VecDeque::new(),
            events: VecDeque::new(),
            duplicates: 0,
        }
    }

    /// How many repeated data frames the device has sent since the last call: frames whose
    /// acknowledgement did not reach it. Each was acknowledged again and otherwise ignored.
    pub fn take_duplicates(&mut self) -> u32 {
        std::mem::take(&mut self.duplicates)
    }

    /// The settings.
    pub fn config(&self) -> &TransportConfig {
        &self.config
    }

    /// Change [`TransportConfig::lost_answer_timeout`] for the requests to come.
    pub fn set_lost_answer_timeout(&mut self, timeout: Option<Duration>) {
        self.config.lost_answer_timeout = timeout;
    }

    /// Change [`TransportConfig::unanswered_timeout`] for the requests to come.
    pub fn set_unanswered_timeout(&mut self, timeout: Option<Duration>) {
        self.config.unanswered_timeout = timeout;
    }

    /// Whether the pending request has been acknowledged by the device and now waits for its
    /// answer: the device has taken it.
    pub fn awaits_answer(&self) -> bool {
        matches!(self.phase, Phase::AwaitingResponse { .. })
    }

    /// Whether the connection is open.
    pub fn is_open(&self) -> bool {
        self.phase != Phase::Closed
    }

    /// Whether a request is pending.
    pub fn is_busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::AwaitingAck { .. } | Phase::AwaitingResponse { .. }
        )
    }

    /// Open the connection: queue a `T_Connect` and start both sequence numbers at 0. A
    /// request still pending on an earlier connection is forgotten.
    pub fn open(&mut self) {
        self.transmits.push_back(Tpdu::Connect);
        self.phase = Phase::Idle;
        self.send_sequence = 0;
        self.receive_sequence = 0;
    }

    /// Close the connection: queue a `T_Disconnect` if it is open. A pending request is
    /// forgotten without an event.
    pub fn close(&mut self) {
        if self.is_open() {
            self.transmits.push_back(Tpdu::Disconnect);
            self.phase = Phase::Closed;
        }
    }

    /// Send `apdu` as numbered data. The outcome arrives as an event:
    /// [`TransportEvent::Acknowledged`] or, with `expects_response`,
    /// [`TransportEvent::Response`]; or [`TransportEvent::Failed`].
    pub fn request(
        &mut self,
        apdu: Apdu,
        expects_response: bool,
        now: Instant,
    ) -> Result<(), RequestError> {
        match self.phase {
            Phase::Closed => return Err(RequestError::NotConnected),
            Phase::Idle => {}
            Phase::AwaitingAck { .. } | Phase::AwaitingResponse { .. } => {
                return Err(RequestError::Busy);
            }
        }
        let sequence = self.send_sequence;
        let frame = Tpdu::Connected { sequence, apdu };
        self.transmits.push_back(frame.clone());
        self.phase = Phase::AwaitingAck {
            frame,
            sequence,
            transmissions: 1,
            deadline: now + self.config.ack_timeout,
            expects_response,
            early_response: None,
        };
        Ok(())
    }

    /// Whether frames wait to be sent.
    pub fn has_transmit(&self) -> bool {
        !self.transmits.is_empty()
    }

    /// The next frame to send to the device.
    pub fn poll_transmit(&mut self) -> Option<Tpdu> {
        self.transmits.pop_front()
    }

    /// The next event.
    pub fn poll_event(&mut self) -> Option<TransportEvent> {
        self.events.pop_front()
    }

    /// When [`Connection::handle_timeout`] should next be called.
    pub fn poll_timeout(&self) -> Option<Instant> {
        match &self.phase {
            Phase::AwaitingAck { deadline, .. } | Phase::AwaitingResponse { deadline, .. } => {
                Some(*deadline)
            }
            Phase::Closed | Phase::Idle => None,
        }
    }

    /// Time has passed: repeat an unacknowledged frame or give up waiting.
    pub fn handle_timeout(&mut self, now: Instant) {
        match &mut self.phase {
            Phase::AwaitingAck { deadline, .. } if now >= *deadline => self.repeat(now),
            Phase::AwaitingResponse { deadline, lost } if now >= *deadline => {
                let failure = if *lost {
                    // Acknowledged unseen: the device counts on, and so does this end.
                    self.transmits.push_back(Tpdu::Ack {
                        sequence: self.receive_sequence,
                    });
                    self.receive_sequence = (self.receive_sequence + 1) & SEQUENCE_MASK;
                    TransportFailure::AnswerLost
                } else {
                    TransportFailure::NoResponse
                };
                self.phase = Phase::Idle;
                self.events.push_back(TransportEvent::Failed(failure));
            }
            _ => {}
        }
    }

    /// Send the pending frame again, or close the connection when the repetitions are used up.
    fn repeat(&mut self, now: Instant) {
        let Phase::AwaitingAck {
            frame,
            transmissions,
            deadline,
            ..
        } = &mut self.phase
        else {
            return;
        };
        if *transmissions > self.config.repetitions {
            self.transmits.push_back(Tpdu::Disconnect);
            self.phase = Phase::Closed;
            self.events
                .push_back(TransportEvent::Failed(TransportFailure::NotAcknowledged));
            return;
        }
        *transmissions += 1;
        *deadline = now + self.config.ack_timeout;
        self.transmits.push_back(frame.clone());
    }

    /// A frame from the device, addressed to the tool, arrived.
    pub fn handle_input(&mut self, tpdu: &Tpdu, now: Instant) {
        if !self.is_open() {
            return;
        }
        match tpdu {
            Tpdu::Disconnect => self.closed_by_device(),
            // The device opening a connection towards the tool ends this one, too.
            Tpdu::Connect => {
                self.transmits.push_back(Tpdu::Disconnect);
                self.closed_by_device();
            }
            Tpdu::Ack { sequence } => self.acknowledged(*sequence, now),
            Tpdu::Nak { sequence } => {
                if matches!(&self.phase, Phase::AwaitingAck { sequence: pending, .. } if pending == sequence)
                {
                    self.repeat(now);
                }
            }
            Tpdu::Connected { sequence, apdu } => self.data(*sequence, apdu),
            // Connectionless frames are not part of the connection.
            Tpdu::Data(_) | Tpdu::TagGroup(_) => {}
        }
    }

    fn closed_by_device(&mut self) {
        let event = if self.is_busy() {
            TransportEvent::Failed(TransportFailure::ClosedByDevice)
        } else {
            TransportEvent::Closed
        };
        self.phase = Phase::Closed;
        self.events.push_back(event);
    }

    fn acknowledged(&mut self, acknowledged: u8, now: Instant) {
        let Phase::AwaitingAck {
            sequence,
            transmissions,
            expects_response,
            early_response,
            ..
        } = &mut self.phase
        else {
            return;
        };
        if *sequence != acknowledged {
            return;
        }
        let repeated = *transmissions > 1;
        let (expects_response, early_response) = (*expects_response, early_response.take());
        self.send_sequence = (self.send_sequence + 1) & SEQUENCE_MASK;
        match (expects_response, early_response) {
            (false, _) => {
                self.phase = Phase::Idle;
                self.events.push_back(TransportEvent::Acknowledged);
            }
            (true, Some(apdu)) => {
                self.phase = Phase::Idle;
                self.events.push_back(TransportEvent::Response(apdu));
            }
            (true, None) => {
                // After a repetition the answer may have been lost with the acknowledgement
                // of the first transmission; where that is watched for, the wait is short.
                let lost = self
                    .config
                    .lost_answer_timeout
                    .filter(|_| repeated)
                    .or(self.config.unanswered_timeout);
                self.phase = Phase::AwaitingResponse {
                    deadline: now + lost.unwrap_or(self.config.response_timeout),
                    lost: lost.is_some(),
                };
            }
        }
    }

    fn data(&mut self, sequence: u8, apdu: &Apdu) {
        let sequence = sequence & SEQUENCE_MASK;
        if sequence == self.receive_sequence.wrapping_sub(1) & SEQUENCE_MASK {
            // A repetition: our acknowledgement did not arrive.
            self.duplicates += 1;
            self.transmits.push_back(Tpdu::Ack { sequence });
            return;
        }
        if sequence != self.receive_sequence {
            if self.phase == Phase::Idle {
                // The two ends have lost step.
                self.transmits.push_back(Tpdu::Disconnect);
                self.phase = Phase::Closed;
                self.events.push_back(TransportEvent::Closed);
            }
            return;
        }
        self.transmits.push_back(Tpdu::Ack { sequence });
        self.receive_sequence = (self.receive_sequence + 1) & SEQUENCE_MASK;
        match &mut self.phase {
            Phase::AwaitingResponse { .. } => {
                self.phase = Phase::Idle;
                self.events
                    .push_back(TransportEvent::Response(apdu.clone()));
            }
            Phase::AwaitingAck {
                expects_response: true,
                early_response: early @ None,
                ..
            } => *early = Some(apdu.clone()),
            _ => self
                .events
                .push_back(TransportEvent::Unsolicited(apdu.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apdu(tag: u8) -> Apdu {
        Apdu::Other {
            apci: 0x3D5,
            data: vec![tag],
        }
    }

    fn open(now: Instant) -> Connection {
        let _ = now;
        let mut connection = Connection::new(TransportConfig::default());
        connection.open();
        assert_eq!(connection.poll_transmit(), Some(Tpdu::Connect));
        connection
    }

    fn drain(connection: &mut Connection) -> Vec<Tpdu> {
        std::iter::from_fn(|| connection.poll_transmit()).collect()
    }

    fn events(connection: &mut Connection) -> Vec<TransportEvent> {
        std::iter::from_fn(|| connection.poll_event()).collect()
    }

    #[test]
    fn request_acknowledged_and_answered() {
        let now = Instant::now();
        let mut connection = open(now);
        connection.request(apdu(1), true, now).unwrap();
        assert_eq!(
            drain(&mut connection),
            [Tpdu::Connected {
                sequence: 0,
                apdu: apdu(1)
            }]
        );
        assert_eq!(connection.poll_timeout(), Some(now + ACK_TIMEOUT));
        assert_eq!(
            connection.request(apdu(2), true, now),
            Err(RequestError::Busy)
        );

        let later = now + Duration::from_millis(40);
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, later);
        assert!(events(&mut connection).is_empty());
        assert_eq!(connection.poll_timeout(), Some(later + RESPONSE_TIMEOUT));

        connection.handle_input(
            &Tpdu::Connected {
                sequence: 0,
                apdu: apdu(9),
            },
            later,
        );
        assert_eq!(drain(&mut connection), [Tpdu::Ack { sequence: 0 }]);
        assert_eq!(events(&mut connection), [TransportEvent::Response(apdu(9))]);
        assert!(!connection.is_busy());
        assert_eq!(connection.poll_timeout(), None);
    }

    #[test]
    fn request_without_answer_completes_on_the_acknowledgement() {
        let now = Instant::now();
        let mut connection = open(now);
        connection.request(apdu(1), false, now).unwrap();
        drain(&mut connection);
        // An acknowledgement for another frame changes nothing.
        connection.handle_input(&Tpdu::Ack { sequence: 5 }, now);
        assert!(events(&mut connection).is_empty());
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
        assert_eq!(events(&mut connection), [TransportEvent::Acknowledged]);
        // The next request carries the next sequence number.
        connection.request(apdu(2), false, now).unwrap();
        assert_eq!(
            drain(&mut connection),
            [Tpdu::Connected {
                sequence: 1,
                apdu: apdu(2)
            }]
        );
    }

    #[test]
    fn lost_acknowledgement_is_repeated_three_times_then_the_connection_closes() {
        let start = Instant::now();
        let mut connection = open(start);
        connection.request(apdu(1), true, start).unwrap();
        let frame = Tpdu::Connected {
            sequence: 0,
            apdu: apdu(1),
        };
        assert_eq!(drain(&mut connection), std::slice::from_ref(&frame));
        let mut now = start;
        for repetition in 1..=3 {
            // Too early: nothing happens.
            connection.handle_timeout(now + Duration::from_millis(2999));
            assert!(drain(&mut connection).is_empty(), "repetition {repetition}");
            now += ACK_TIMEOUT;
            connection.handle_timeout(now);
            assert_eq!(drain(&mut connection), std::slice::from_ref(&frame));
            assert!(events(&mut connection).is_empty());
        }
        now += ACK_TIMEOUT;
        connection.handle_timeout(now);
        assert_eq!(drain(&mut connection), [Tpdu::Disconnect]);
        assert_eq!(
            events(&mut connection),
            [TransportEvent::Failed(TransportFailure::NotAcknowledged)]
        );
        assert!(!connection.is_open());
        assert_eq!(
            connection.request(apdu(2), true, now),
            Err(RequestError::NotConnected)
        );
    }

    #[test]
    fn a_repetition_that_is_acknowledged_carries_on() {
        let start = Instant::now();
        let mut connection = open(start);
        connection.request(apdu(1), true, start).unwrap();
        drain(&mut connection);
        let now = start + ACK_TIMEOUT;
        connection.handle_timeout(now);
        assert_eq!(drain(&mut connection).len(), 1);
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 0,
                apdu: apdu(7),
            },
            now,
        );
        assert_eq!(events(&mut connection), [TransportEvent::Response(apdu(7))]);
    }

    #[test]
    fn an_answer_that_overtakes_the_acknowledgement_is_kept() {
        let start = Instant::now();
        let mut connection = open(start);
        connection.request(apdu(1), true, start).unwrap();
        drain(&mut connection);
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 0,
                apdu: apdu(7),
            },
            start,
        );
        assert_eq!(drain(&mut connection), [Tpdu::Ack { sequence: 0 }]);
        assert!(events(&mut connection).is_empty());
        assert!(connection.is_busy());
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, start);
        assert_eq!(events(&mut connection), [TransportEvent::Response(apdu(7))]);
    }

    #[test]
    fn repeated_data_is_acknowledged_again_and_dropped() {
        let now = Instant::now();
        let mut connection = open(now);
        connection.request(apdu(1), true, now).unwrap();
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
        let answer = Tpdu::Connected {
            sequence: 0,
            apdu: apdu(7),
        };
        connection.handle_input(&answer, now);
        drain(&mut connection);
        assert_eq!(events(&mut connection).len(), 1);
        // The device did not get our acknowledgement and repeats its answer.
        assert_eq!(connection.take_duplicates(), 0);
        connection.handle_input(&answer, now);
        assert_eq!(drain(&mut connection), [Tpdu::Ack { sequence: 0 }]);
        assert!(events(&mut connection).is_empty());
        assert!(connection.is_open());
        assert_eq!(connection.take_duplicates(), 1);
        assert_eq!(connection.take_duplicates(), 0);
    }

    #[test]
    fn missing_answer_fails_the_request_and_keeps_the_connection() {
        let start = Instant::now();
        let mut connection = open(start);
        connection.request(apdu(1), true, start).unwrap();
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, start);
        drain(&mut connection);
        connection.handle_timeout(start + RESPONSE_TIMEOUT - Duration::from_millis(1));
        assert!(events(&mut connection).is_empty());
        connection.handle_timeout(start + RESPONSE_TIMEOUT);
        assert_eq!(
            events(&mut connection),
            [TransportEvent::Failed(TransportFailure::NoResponse)]
        );
        assert!(connection.is_open());
        assert!(drain(&mut connection).is_empty());
        // The retry uses the next sequence number.
        connection
            .request(apdu(1), true, start + RESPONSE_TIMEOUT)
            .unwrap();
        assert_eq!(
            drain(&mut connection),
            [Tpdu::Connected {
                sequence: 1,
                apdu: apdu(1)
            }]
        );
    }

    #[test]
    fn negative_acknowledgement_repeats_at_once() {
        let now = Instant::now();
        let mut connection = open(now);
        connection.request(apdu(1), false, now).unwrap();
        let frame = drain(&mut connection);
        connection.handle_input(&Tpdu::Nak { sequence: 3 }, now);
        assert!(drain(&mut connection).is_empty());
        for _ in 0..3 {
            connection.handle_input(&Tpdu::Nak { sequence: 0 }, now);
            assert_eq!(drain(&mut connection), frame);
        }
        connection.handle_input(&Tpdu::Nak { sequence: 0 }, now);
        assert_eq!(drain(&mut connection), [Tpdu::Disconnect]);
        assert_eq!(
            events(&mut connection),
            [TransportEvent::Failed(TransportFailure::NotAcknowledged)]
        );
    }

    #[test]
    fn disconnect_from_the_device() {
        let now = Instant::now();
        let mut connection = open(now);
        connection.handle_input(&Tpdu::Disconnect, now);
        assert_eq!(events(&mut connection), [TransportEvent::Closed]);
        assert!(drain(&mut connection).is_empty());
        // Frames for a closed connection are ignored.
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
        connection.handle_input(&Tpdu::Disconnect, now);
        assert!(events(&mut connection).is_empty());

        let mut connection = open(now);
        connection.request(apdu(1), true, now).unwrap();
        drain(&mut connection);
        connection.handle_input(&Tpdu::Disconnect, now);
        assert_eq!(
            events(&mut connection),
            [TransportEvent::Failed(TransportFailure::ClosedByDevice)]
        );
        assert_eq!(connection.poll_timeout(), None);
    }

    #[test]
    fn data_out_of_step() {
        let now = Instant::now();
        let mut connection = open(now);
        // While a request is pending it is dropped without an acknowledgement.
        connection.request(apdu(1), true, now).unwrap();
        drain(&mut connection);
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 4,
                apdu: apdu(7),
            },
            now,
        );
        assert!(drain(&mut connection).is_empty());
        assert!(connection.is_busy());
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 0,
                apdu: apdu(8),
            },
            now,
        );
        drain(&mut connection);
        events(&mut connection);
        // While nothing is pending it closes the connection.
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 4,
                apdu: apdu(7),
            },
            now,
        );
        assert_eq!(drain(&mut connection), [Tpdu::Disconnect]);
        assert_eq!(events(&mut connection), [TransportEvent::Closed]);
    }

    #[test]
    fn unsolicited_data_is_acknowledged_and_reported() {
        let now = Instant::now();
        let mut connection = open(now);
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 0,
                apdu: apdu(7),
            },
            now,
        );
        assert_eq!(drain(&mut connection), [Tpdu::Ack { sequence: 0 }]);
        assert_eq!(
            events(&mut connection),
            [TransportEvent::Unsolicited(apdu(7))]
        );
    }

    #[test]
    fn sequence_numbers_wrap_after_sixteen() {
        let now = Instant::now();
        let mut connection = open(now);
        for round in 0u8..40 {
            let sequence = round % 16;
            connection.request(apdu(round), true, now).unwrap();
            assert_eq!(
                drain(&mut connection),
                [Tpdu::Connected {
                    sequence,
                    apdu: apdu(round)
                }]
            );
            connection.handle_input(&Tpdu::Ack { sequence }, now);
            connection.handle_input(
                &Tpdu::Connected {
                    sequence,
                    apdu: apdu(round),
                },
                now,
            );
            assert_eq!(drain(&mut connection), [Tpdu::Ack { sequence }]);
            assert_eq!(
                events(&mut connection),
                [TransportEvent::Response(apdu(round))]
            );
        }
    }

    #[test]
    fn close_and_reopen() {
        let now = Instant::now();
        let mut connection = open(now);
        connection.request(apdu(1), false, now).unwrap();
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
        drain(&mut connection);
        events(&mut connection);
        connection.close();
        connection.close();
        assert_eq!(drain(&mut connection), [Tpdu::Disconnect]);
        assert!(!connection.is_open());
        connection.open();
        connection.request(apdu(2), false, now).unwrap();
        assert_eq!(
            drain(&mut connection),
            [
                Tpdu::Connect,
                Tpdu::Connected {
                    sequence: 0,
                    apdu: apdu(2)
                }
            ]
        );
    }

    /// A connection that watches for answers lost with their acknowledgement.
    fn watchful(now: Instant) -> Connection {
        let mut connection = open(now);
        connection.set_lost_answer_timeout(Some(Duration::from_secs(1)));
        connection
    }

    #[test]
    fn an_answer_lost_with_its_acknowledgement_is_acknowledged_unseen() {
        let now = Instant::now();
        let mut connection = watchful(now);
        connection.request(apdu(1), true, now).unwrap();
        drain(&mut connection);
        // Nothing comes back: the request goes out again, and that is acknowledged.
        let repeated = now + ACK_TIMEOUT;
        connection.handle_timeout(repeated);
        assert_eq!(drain(&mut connection).len(), 1);
        let acknowledged = repeated + Duration::from_millis(90);
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, acknowledged);
        assert!(events(&mut connection).is_empty());
        // The answer is given one second, not ten.
        let given_up = acknowledged + Duration::from_secs(1);
        assert_eq!(connection.poll_timeout(), Some(given_up));
        connection.handle_timeout(given_up - Duration::from_millis(1));
        assert!(events(&mut connection).is_empty());
        connection.handle_timeout(given_up);
        // The answer nobody saw is acknowledged with the number it must have had.
        assert_eq!(drain(&mut connection), [Tpdu::Ack { sequence: 0 }]);
        assert_eq!(
            events(&mut connection),
            [TransportEvent::Failed(TransportFailure::AnswerLost)]
        );
        assert!(connection.is_open() && !connection.is_busy());

        // The same request as a new one: the next number out, and the next number back.
        connection.request(apdu(1), true, given_up).unwrap();
        assert_eq!(
            drain(&mut connection),
            [Tpdu::Connected {
                sequence: 1,
                apdu: apdu(1)
            }]
        );
        connection.handle_input(&Tpdu::Ack { sequence: 1 }, given_up);
        // Acknowledged at the first go: this answer has the usual ten seconds.
        assert_eq!(connection.poll_timeout(), Some(given_up + RESPONSE_TIMEOUT));
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 1,
                apdu: apdu(9),
            },
            given_up,
        );
        assert_eq!(drain(&mut connection), [Tpdu::Ack { sequence: 1 }]);
        assert_eq!(events(&mut connection), [TransportEvent::Response(apdu(9))]);
    }

    #[test]
    fn an_answer_that_comes_after_it_was_given_up_is_a_repetition() {
        let now = Instant::now();
        let mut connection = watchful(now);
        connection.request(apdu(1), true, now).unwrap();
        connection.handle_timeout(now + ACK_TIMEOUT);
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now + ACK_TIMEOUT);
        let given_up = now + ACK_TIMEOUT + Duration::from_secs(1);
        connection.handle_timeout(given_up);
        drain(&mut connection);
        events(&mut connection);
        connection.request(apdu(1), true, given_up).unwrap();
        drain(&mut connection);
        // The device was only slow: its answer to the first request arrives now. It is one
        // behind, so it is acknowledged again and not taken for the answer to the second.
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 0,
                apdu: apdu(8),
            },
            given_up,
        );
        assert_eq!(drain(&mut connection), [Tpdu::Ack { sequence: 0 }]);
        assert!(events(&mut connection).is_empty());
        assert_eq!(connection.take_duplicates(), 1);
        connection.handle_input(&Tpdu::Ack { sequence: 1 }, given_up);
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 1,
                apdu: apdu(9),
            },
            given_up,
        );
        assert_eq!(events(&mut connection), [TransportEvent::Response(apdu(9))]);
    }

    #[test]
    fn answers_are_not_given_up_early_unless_asked_for() {
        let now = Instant::now();
        // By default a repetition changes nothing: ten seconds, then "no answer", and no
        // acknowledgement of anything unseen.
        let mut connection = open(now);
        connection.request(apdu(1), true, now).unwrap();
        connection.handle_timeout(now + ACK_TIMEOUT);
        drain(&mut connection);
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now + ACK_TIMEOUT);
        assert_eq!(
            connection.poll_timeout(),
            Some(now + ACK_TIMEOUT + RESPONSE_TIMEOUT)
        );
        connection.handle_timeout(now + ACK_TIMEOUT + RESPONSE_TIMEOUT);
        assert!(drain(&mut connection).is_empty());
        assert_eq!(
            events(&mut connection),
            [TransportEvent::Failed(TransportFailure::NoResponse)]
        );

        // Asked for, it still needs a repetition: a request acknowledged at the first go
        // waits the full time, and so does one without an answer.
        let mut connection = watchful(now);
        connection.request(apdu(1), true, now).unwrap();
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
        assert_eq!(connection.poll_timeout(), Some(now + RESPONSE_TIMEOUT));
        // And an answer that does follow the repetition is simply the answer.
        let mut connection = watchful(now);
        connection.request(apdu(1), true, now).unwrap();
        connection.handle_timeout(now + ACK_TIMEOUT);
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now + ACK_TIMEOUT);
        connection.handle_input(
            &Tpdu::Connected {
                sequence: 0,
                apdu: apdu(9),
            },
            now + ACK_TIMEOUT,
        );
        assert_eq!(events(&mut connection), [TransportEvent::Response(apdu(9))]);
    }

    #[test]
    fn an_unanswered_request_can_be_given_up_while_the_connection_lasts() {
        let now = Instant::now();
        let mut connection = open(now);
        connection.set_unanswered_timeout(Some(Duration::from_secs(4)));
        connection.request(apdu(1), true, now).unwrap();
        drain(&mut connection);
        // Acknowledged at the first go: the device has the request. No answer follows.
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
        assert!(connection.awaits_answer());
        assert_eq!(
            connection.poll_timeout(),
            Some(now + Duration::from_secs(4))
        );
        connection.handle_timeout(now + Duration::from_secs(4));
        assert_eq!(drain(&mut connection), [Tpdu::Ack { sequence: 0 }]);
        assert_eq!(
            events(&mut connection),
            [TransportEvent::Failed(TransportFailure::AnswerLost)]
        );
        assert!(connection.is_open() && !connection.awaits_answer());

        // After a repetition the shorter wait applies where one is set.
        connection.set_lost_answer_timeout(Some(Duration::from_secs(1)));
        let later = now + Duration::from_secs(5);
        connection.request(apdu(2), true, later).unwrap();
        connection.handle_timeout(later + ACK_TIMEOUT);
        connection.handle_input(&Tpdu::Ack { sequence: 1 }, later + ACK_TIMEOUT);
        assert_eq!(
            connection.poll_timeout(),
            Some(later + ACK_TIMEOUT + Duration::from_secs(1))
        );
        // A request without an answer is done with its acknowledgement, as ever.
        let mut connection = open(now);
        connection.set_unanswered_timeout(Some(Duration::from_secs(4)));
        connection.request(apdu(3), false, now).unwrap();
        connection.handle_input(&Tpdu::Ack { sequence: 0 }, now);
        assert_eq!(events(&mut connection), [TransportEvent::Acknowledged]);
    }
}
