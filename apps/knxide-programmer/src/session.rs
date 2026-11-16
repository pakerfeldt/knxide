//! One transport connection to one device, driven over a [`Link`]: the place where the
//! planner's requests become frames and the device's frames become answers.
//!
//! The rules of the connection are in [`knxide_program::transport`]; this module owns the
//! timers and the waiting.

use std::time::Duration;

use knxide_core::IndividualAddress;
use knxide_program::transport::{Connection, TransportConfig, TransportEvent, TransportFailure};
use std::fmt;

use knxide_program::{Expect, Request, Response, wire};
use tokio::time::Instant;

use crate::link::{Link, LinkError};

/// Settings of a [`Session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    /// Timers and repetitions of the transport layer.
    pub transport: TransportConfig,
    /// How often a request is sent again, as a new frame, when its answer does not come.
    /// Behaviour notes on the recorded tool say once; a read-only comparison reconnects
    /// instead and uses 0.
    pub response_retries: u8,
    /// Give a connection up when nothing has been heard from the device for this long, in
    /// place of repeating into it or sending the next request on it: a device drops a
    /// connection that has been idle for about six seconds, and what it then makes of further
    /// frames is anybody's guess. `None` keeps repeating as the transport layer prescribes;
    /// that is what a download does, which must not start over on its own.
    pub stale_after: Option<Duration>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            transport: TransportConfig::default(),
            response_retries: 1,
            stale_after: None,
        }
    }
}

/// Why a session could not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionFault {
    /// Something between the tool and this device went wrong: no acknowledgement, the device
    /// closed the connection, the connection went silent. A new connection may work.
    Device(String),
    /// The access to the bus itself is gone. Nothing more can be sent.
    Link(String),
    /// The device acknowledged the request and its answer did not arrive; the answer was
    /// acknowledged unseen and the connection is still open. Only a session told not to ask
    /// again by itself reports this: see [`Session::set_ask_again`].
    AnswerLost(String),
}

impl fmt::Display for SessionFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(what) | Self::Link(what) | Self::AnswerLost(what) => f.write_str(what),
        }
    }
}

fn device(what: impl Into<String>) -> SessionFault {
    SessionFault::Device(what.into())
}

fn link_lost() -> SessionFault {
    SessionFault::Link("the bus connection was lost".into())
}

/// A transport connection to one device.
pub struct Session<L: Link> {
    link: L,
    device: IndividualAddress,
    connection: Connection,
    config: SessionConfig,
    notes: Vec<String>,
    /// When the device was last heard (or the connection opened).
    last_heard: Instant,
    /// Transmissions of numbered frames beyond the first of each.
    repetitions: u32,
    /// Requests made again on the same connection because their answer was lost.
    re_requests: u32,
    /// Whether a request whose answer is taken as lost is made again by the session itself.
    ask_again: bool,
    /// Whether the device acknowledged the request of the last exchange.
    acknowledged: bool,
}

/// The clock of the connection: tokio's, so that tests can run on paused time.
fn now() -> std::time::Instant {
    Instant::now().into_std()
}

impl<L: Link> Session<L> {
    /// A closed connection to `device` over `link`.
    pub fn new(link: L, device: IndividualAddress, config: SessionConfig) -> Self {
        Self {
            link,
            device,
            connection: Connection::new(config.transport),
            config,
            notes: Vec::new(),
            last_heard: Instant::now(),
            repetitions: 0,
            re_requests: 0,
            ask_again: true,
            acknowledged: false,
        }
    }

    /// The device this session talks to.
    pub fn device(&self) -> IndividualAddress {
        self.device
    }

    /// Whether the transport connection is open.
    pub fn is_open(&self) -> bool {
        self.connection.is_open()
    }

    /// Change how often an unanswered request is sent again.
    pub fn set_response_retries(&mut self, retries: u8) {
        self.config.response_retries = retries;
    }

    /// Change after how long a silence the connection is given up; see
    /// [`SessionConfig::stale_after`].
    pub fn set_stale_after(&mut self, stale_after: Option<Duration>) {
        self.config.stale_after = stale_after;
    }

    /// Watch for answers that were lost together with the acknowledgement of their request,
    /// and ask again on the same connection instead of waiting for the device to close it:
    /// see [`TransportConfig::lost_answer_timeout`]. A request whose repetition is
    /// acknowledged and whose answer does not follow within `after` has that answer
    /// acknowledged unseen and is made once more as a new request.
    ///
    /// For sessions that only read. A download leaves this off: a write must not be made a
    /// second time on a guess about what the device did with the first.
    pub fn set_lost_answer_after(&mut self, after: Option<Duration>) {
        self.config.transport.lost_answer_timeout = after;
        self.connection.set_lost_answer_timeout(after);
    }

    /// Like [`Session::set_lost_answer_after`], for a request acknowledged at the first go:
    /// see [`TransportConfig::unanswered_timeout`].
    pub fn set_unanswered_after(&mut self, after: Option<Duration>) {
        self.config.transport.unanswered_timeout = after;
        self.connection.set_unanswered_timeout(after);
    }

    /// Whether a request whose answer was taken as lost is made again by the session (the
    /// default, right for reads) or reported as [`SessionFault::AnswerLost`], for a caller
    /// that must not make a write twice and looks instead at what the device holds.
    pub fn set_ask_again(&mut self, ask_again: bool) {
        self.ask_again = ask_again;
    }

    /// Whether the device acknowledged the request of the last exchange: after an exchange
    /// that failed, whether the device is known to have taken the request.
    pub fn was_acknowledged(&self) -> bool {
        self.acknowledged
    }

    /// How many requests were made again on the same connection since the last call, their
    /// first answer having been lost.
    pub fn take_re_requests(&mut self) -> u32 {
        std::mem::take(&mut self.re_requests)
    }

    /// How many frames had to be sent again since the last call, by either side: each
    /// transmission of a numbered frame after its first, and each answer the device repeated
    /// because the acknowledgement did not reach it.
    pub fn take_repetitions(&mut self) -> u32 {
        std::mem::take(&mut self.repetitions) + self.connection.take_duplicates()
    }

    /// Close the connection without waiting for anything: a `T_Disconnect` goes out if the
    /// connection is open.
    pub async fn close(&mut self) {
        self.connection.close();
        let _ = self.flush().await;
        while self.connection.poll_event().is_some() {}
    }

    /// A frame from the bus: taken in if it comes from this session's device.
    fn heard(&mut self, source: IndividualAddress, tpdu: &knxide_core::Tpdu) {
        if source == self.device {
            self.last_heard = Instant::now();
            self.connection.handle_input(tpdu, now());
        }
    }

    /// When the connection counts as given up by the device, if that is watched.
    fn stale_at(&self) -> Option<Instant> {
        self.config.stale_after.map(|limit| self.last_heard + limit)
    }

    /// Things that happened below the level of requests since the last call: repetitions,
    /// frames nobody asked for.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Give the link back, closing the connection first if it is still open.
    pub async fn finish(mut self) -> L {
        if self.connection.is_open() {
            self.connection.close();
            let _ = self.flush().await;
        }
        self.link
    }

    /// Send what the connection has queued. A frame nobody acknowledged on the bus is left to
    /// the transport layer's repetitions, except the connect: without a device there is
    /// nothing to wait for.
    async fn flush(&mut self) -> Result<(), SessionFault> {
        while let Some(tpdu) = self.connection.poll_transmit() {
            let connect = tpdu == knxide_core::Tpdu::Connect;
            match self.link.send(self.device, tpdu).await {
                Ok(()) => {}
                Err(LinkError::NotAcknowledged) if connect => {
                    self.connection.close();
                    while self.connection.poll_transmit().is_some() {}
                    return Err(device(format!("no device answers at {}", self.device)));
                }
                Err(LinkError::NotAcknowledged) => {
                    self.notes
                        .push("a frame was not acknowledged on the bus".into());
                }
                Err(LinkError::Failed(why)) => {
                    return Err(SessionFault::Link(format!(
                        "the bus connection failed: {why}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Take in frames that arrived while nothing was waited for.
    async fn drain(&mut self) -> Result<(), SessionFault> {
        loop {
            let frame = tokio::select! {
                biased;
                frame = self.link.receive() => frame,
                () = std::future::ready(()) => return Ok(()),
            };
            match frame {
                Some((source, tpdu)) => self.heard(source, &tpdu),
                None => return Err(link_lost()),
            }
        }
    }

    /// Events that are not the outcome of a request. Returns an error when the connection
    /// closed.
    fn idle_events(&mut self) -> Result<(), SessionFault> {
        while let Some(event) = self.connection.poll_event() {
            match event {
                TransportEvent::Closed => {
                    return Err(device("the device closed the connection"));
                }
                TransportEvent::Unsolicited(apdu) => self.notes.push(format!(
                    "the device sent a frame nobody asked for: {apdu:?}"
                )),
                other => self
                    .notes
                    .push(format!("unexpected transport event {other:?}")),
            }
        }
        Ok(())
    }

    /// Let `duration` pass, answering the device's frames meanwhile.
    pub async fn pause(&mut self, duration: Duration) -> Result<(), SessionFault> {
        let until = Instant::now() + duration;
        loop {
            self.flush().await?;
            self.idle_events()?;
            tokio::select! {
                frame = self.link.receive() => match frame {
                    Some((source, tpdu)) => self.heard(source, &tpdu),
                    None => return Err(link_lost()),
                },
                () = tokio::time::sleep_until(until) => return Ok(()),
            }
        }
    }

    /// Hand one request to the device and wait for what the planner expects: its answer, or
    /// its acknowledgement. `Ok(None)` for a request without answer, and for an answer that
    /// did not come in time.
    pub async fn exchange(
        &mut self,
        request: &Request,
        expect: Expect,
    ) -> Result<Option<Response>, SessionFault> {
        match request {
            Request::Connect => {
                self.connection.open();
                while self.connection.poll_event().is_some() {}
                self.last_heard = Instant::now();
                self.flush().await?;
                return Ok(None);
            }
            Request::Disconnect => {
                self.connection.close();
                // The device may be gone already (after a restart); nothing depends on this
                // frame arriving.
                let _ = self.flush().await;
                return Ok(None);
            }
            _ => {}
        }
        self.drain().await?;
        self.flush().await?;
        self.idle_events()?;
        if self.connection.is_open() && self.stale_at().is_some_and(|at| Instant::now() >= at) {
            self.close().await;
            return Err(device(
                "the connection has been silent for longer than a device keeps one open",
            ));
        }

        let apdu = wire::request_apdu(request).map_err(|error| device(error.to_string()))?;
        let expects_response = expect == Expect::Answer;
        let restart = *request == Request::Restart;
        let mut retries = self.config.response_retries;
        // An answer taken as lost is asked for again once; a second loss on the same request
        // is left to the caller, who can open a new connection.
        let mut asked_again = false;
        self.acknowledged = false;
        self.connection
            .request(apdu.clone(), expects_response, now())
            .map_err(|error| device(error.to_string()))?;
        let mut transmissions = 0;
        loop {
            while let Some(tpdu) = self.connection.poll_transmit() {
                if matches!(tpdu, knxide_core::Tpdu::Connected { .. }) {
                    transmissions += 1;
                    if transmissions > 1 {
                        self.repetitions += 1;
                        self.notes.push(format!(
                            "{request}: sent again (transmission {transmissions})"
                        ));
                    }
                }
                match self.link.send(self.device, tpdu).await {
                    Ok(()) => {}
                    Err(LinkError::NotAcknowledged) => self
                        .notes
                        .push(format!("{request}: not acknowledged on the bus")),
                    Err(LinkError::Failed(why)) => {
                        return Err(SessionFault::Link(format!(
                            "the bus connection failed: {why}"
                        )));
                    }
                }
            }
            if self.connection.awaits_answer() {
                self.acknowledged = true;
            }
            while let Some(event) = self.connection.poll_event() {
                match event {
                    TransportEvent::Acknowledged => {
                        self.acknowledged = true;
                        return Ok(None);
                    }
                    TransportEvent::Response(apdu) => {
                        self.acknowledged = true;
                        return match wire::response_from_apdu(&apdu) {
                            Some(response) => Ok(Some(response)),
                            None => Err(device(format!(
                                "the device answered {request} with something else: {apdu:?}"
                            ))),
                        };
                    }
                    // A device that restarts may not get round to acknowledging the request.
                    TransportEvent::Failed(
                        TransportFailure::NotAcknowledged | TransportFailure::ClosedByDevice,
                    ) if restart => return Ok(None),
                    TransportEvent::Failed(TransportFailure::NoResponse) => {
                        if retries == 0 {
                            return Ok(None);
                        }
                        retries -= 1;
                        transmissions = 0;
                        self.notes
                            .push(format!("{request}: no answer, asking once more"));
                        self.connection
                            .request(apdu.clone(), expects_response, now())
                            .map_err(|error| device(error.to_string()))?;
                    }
                    TransportEvent::Failed(TransportFailure::AnswerLost) if !self.ask_again => {
                        self.acknowledged = true;
                        return Err(SessionFault::AnswerLost(format!(
                            "{request}: the device acknowledged the request and its answer \
                             did not arrive"
                        )));
                    }
                    TransportEvent::Failed(TransportFailure::AnswerLost) if !asked_again => {
                        asked_again = true;
                        transmissions = 0;
                        self.re_requests += 1;
                        self.notes.push(format!(
                            "{request}: the repetition was acknowledged and no answer \
                             followed; taking the answer as lost, acknowledging it and asking \
                             again"
                        ));
                        self.connection
                            .request(apdu.clone(), expects_response, now())
                            .map_err(|error| device(error.to_string()))?;
                    }
                    TransportEvent::Failed(failure) => {
                        return Err(device(format!("{request}: {failure}")));
                    }
                    TransportEvent::Closed => {
                        return Err(device("the device closed the connection"));
                    }
                    TransportEvent::Unsolicited(apdu) => self.notes.push(format!(
                        "the device sent a frame nobody asked for: {apdu:?}"
                    )),
                }
            }
            if self.connection.has_transmit() {
                continue;
            }
            let Some(deadline) = self.connection.poll_timeout() else {
                return Err(device(format!("{request}: the connection stopped waiting")));
            };
            let deadline = Instant::from_std(deadline);
            // Where a silent connection is given up, that comes before the next repetition.
            let stale = self.stale_at().filter(|at| *at <= deadline);
            tokio::select! {
                frame = self.link.receive() => match frame {
                    Some((source, tpdu)) => self.heard(source, &tpdu),
                    None => return Err(link_lost()),
                },
                () = tokio::time::sleep_until(stale.unwrap_or(deadline)) => {
                    if stale.is_some() {
                        self.close().await;
                        return Err(device(format!(
                            "{request}: nothing heard from the device for longer than it \
                             keeps a connection open"
                        )));
                    }
                    self.connection.handle_timeout(now());
                }
            }
        }
    }
}
