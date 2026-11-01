//! A minimal blocking driver for [`TunnelClient`] over a std UDP socket, for tests that do
//! not want an async runtime.

use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use knxide_core::Cemi;
use knxide_ip::tunnel::{Event, SendError, SendId, TunnelClient, TunnelConfig};

/// A [`TunnelClient`] driven by a loopback socket.
pub struct BlockingTunnel {
    /// The state machine.
    pub client: TunnelClient,
    socket: UdpSocket,
    events: VecDeque<Event>,
}

impl BlockingTunnel {
    /// Bind a loopback socket and start connecting.
    pub fn connect(config: TunnelConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        let mut tunnel = Self {
            client: TunnelClient::new(config, Instant::now()),
            socket,
            events: VecDeque::new(),
        };
        tunnel.flush()?;
        Ok(tunnel)
    }

    /// The local socket address.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Queue a frame.
    pub fn send(&mut self, cemi: &Cemi) -> Result<SendId, SendError> {
        let id = self.client.send(cemi, Instant::now())?;
        let _ = self.flush();
        Ok(id)
    }

    /// Close the tunnel.
    pub fn close(&mut self) {
        self.client.close(Instant::now());
        let _ = self.flush();
    }

    fn flush(&mut self) -> io::Result<()> {
        while let Some(tx) = self.client.poll_transmit() {
            self.socket.send_to(&tx.payload, tx.destination)?;
        }
        while let Some(event) = self.client.poll_event() {
            self.events.push_back(event);
        }
        Ok(())
    }

    /// Wait for one datagram or timer, at most `max_wait`.
    pub fn step(&mut self, max_wait: Duration) -> io::Result<()> {
        let now = Instant::now();
        let wait = self
            .client
            .poll_timeout()
            .map_or(max_wait, |t| t.saturating_duration_since(now).min(max_wait));
        self.socket
            .set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
        let mut buf = [0u8; 1024];
        match self.socket.recv_from(&mut buf) {
            Ok((len, source)) => {
                if let Some(datagram) = buf.get(..len) {
                    self.client.handle_input(source, datagram, Instant::now());
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
        if self
            .client
            .poll_timeout()
            .is_some_and(|t| t <= Instant::now())
        {
            self.client.handle_timeout(Instant::now());
        }
        self.flush()
    }

    /// Drive the client until an event matches `pred` or `timeout` passes. Events that do not
    /// match are discarded.
    pub fn wait_for(
        &mut self,
        mut pred: impl FnMut(&Event) -> bool,
        timeout: Duration,
    ) -> Option<Event> {
        let deadline = Instant::now() + timeout;
        loop {
            while let Some(event) = self.events.pop_front() {
                if pred(&event) {
                    return Some(event);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            self.step(deadline - now).ok()?;
        }
    }

    /// Drive the client until the given send completes.
    pub fn wait_for_send(
        &mut self,
        id: SendId,
        timeout: Duration,
    ) -> Option<Result<(), SendError>> {
        match self.wait_for(
            |e| matches!(e, Event::SendResult { id: i, .. } if *i == id),
            timeout,
        )? {
            Event::SendResult { result, .. } => Some(result),
            _ => None,
        }
    }
}
