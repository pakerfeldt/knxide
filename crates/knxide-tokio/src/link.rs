//! The byte transport under the tunnel driver: a UDP socket, or a TCP stream that is opened
//! on demand and split into KNXnet/IP frames by their header length.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use knxide_ip::frame::frame_len;
use knxide_ip::tunnel::Transmit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

pub(crate) enum Received {
    Frame(SocketAddr, Vec<u8>),
    /// The TCP stream ended or carried garbage.
    Closed,
}

pub(crate) enum Link {
    Udp { socket: UdpSocket, buf: Vec<u8> },
    Tcp(TcpLink),
}

pub(crate) struct TcpLink {
    gateway: SocketAddr,
    connect_timeout: Duration,
    stream: Option<TcpStream>,
    buf: Vec<u8>,
}

impl Link {
    pub(crate) fn udp(socket: UdpSocket) -> Self {
        Self::Udp {
            socket,
            buf: vec![0; 2048],
        }
    }

    pub(crate) fn tcp(gateway: SocketAddr, connect_timeout: Duration) -> Self {
        Self::Tcp(TcpLink {
            gateway,
            connect_timeout,
            stream: None,
            buf: Vec::new(),
        })
    }

    pub(crate) fn is_tcp(&self) -> bool {
        matches!(self, Self::Tcp(_))
    }

    /// Drop the TCP stream; the next send opens a new one.
    pub(crate) fn reset(&mut self) {
        if let Self::Tcp(link) = self {
            link.stream = None;
            link.buf.clear();
        }
    }

    pub(crate) async fn send(&mut self, tx: &Transmit) -> io::Result<()> {
        match self {
            Self::Udp { socket, .. } => socket.send_to(&tx.payload, tx.destination).await.map(drop),
            Self::Tcp(link) => {
                if link.stream.is_none() {
                    let connect = TcpStream::connect(link.gateway);
                    let stream = tokio::time::timeout(link.connect_timeout, connect)
                        .await
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::TimedOut, "TCP connect timed out")
                        })??;
                    stream.set_nodelay(true)?;
                    link.buf.clear();
                    link.stream = Some(stream);
                }
                match &mut link.stream {
                    Some(stream) => stream.write_all(&tx.payload).await,
                    None => Err(io::ErrorKind::NotConnected.into()),
                }
            }
        }
    }

    /// The next datagram or frame. Cancel-safe: partial TCP reads stay buffered.
    pub(crate) async fn recv(&mut self) -> io::Result<Received> {
        match self {
            Self::Udp { socket, buf } => {
                let (len, source) = socket.recv_from(buf).await?;
                Ok(Received::Frame(
                    source,
                    buf.get(..len).unwrap_or_default().to_vec(),
                ))
            }
            Self::Tcp(link) => {
                let Some(stream) = &mut link.stream else {
                    return std::future::pending().await;
                };
                loop {
                    match frame_len(&link.buf) {
                        Ok(Some(len)) if link.buf.len() >= len => {
                            let frame = link.buf.drain(..len).collect();
                            return Ok(Received::Frame(link.gateway, frame));
                        }
                        Ok(_) => {}
                        Err(err) => {
                            tracing::debug!(%err, "bad frame on TCP stream");
                            return Ok(Received::Closed);
                        }
                    }
                    let mut chunk = [0u8; 1024];
                    let n = stream.read(&mut chunk).await?;
                    if n == 0 {
                        return Ok(Received::Closed);
                    }
                    link.buf
                        .extend_from_slice(chunk.get(..n).unwrap_or_default());
                }
            }
        }
    }
}
