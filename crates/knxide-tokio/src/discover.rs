//! Async gateway discovery.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

use knxide_ip::Hpai;
use knxide_ip::discovery::{Discovery, Gateway, MULTICAST_ENDPOINT};
use tokio::net::UdpSocket;

use crate::error::Error;

/// Search the local network for KNXnet/IP servers by multicast for `timeout`.
///
/// Uses the interface the OS routes `224.0.23.12` through. If multicast does not reach the
/// gateway (other subnet, VPN, container), use [`describe`] with its address instead.
pub async fn discover(timeout: Duration) -> Result<Vec<Gateway>, Error> {
    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
    // Not every server honours route-back for searches, so advertise our real address.
    let local = Hpai::udp(local_endpoint(&socket, MULTICAST_ENDPOINT).await?);
    run(
        socket,
        Discovery::search(local, timeout, Instant::now()),
        false,
    )
    .await
}

/// Ask the server at `target` to describe itself. Returns `None` if it does not answer within
/// `timeout`.
pub async fn describe(target: SocketAddr, timeout: Duration) -> Result<Option<Gateway>, Error> {
    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
    let gateways = run(
        socket,
        Discovery::describe(target, Hpai::ROUTE_BACK, timeout, Instant::now()),
        true,
    )
    .await?;
    Ok(gateways.into_iter().next())
}

async fn local_endpoint(socket: &UdpSocket, towards: SocketAddr) -> Result<SocketAddrV4, Error> {
    let probe = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
    probe.connect(towards).await?;
    let (SocketAddr::V4(local), SocketAddr::V4(bound)) =
        (probe.local_addr()?, socket.local_addr()?)
    else {
        return Err(Error::Unsupported("IPv6"));
    };
    Ok(SocketAddrV4::new(*local.ip(), bound.port()))
}

async fn run(
    socket: UdpSocket,
    mut discovery: Discovery,
    stop_after_first: bool,
) -> Result<Vec<Gateway>, Error> {
    while let Some(tx) = discovery.poll_transmit() {
        socket.send_to(&tx.payload, tx.destination).await?;
    }
    let deadline = tokio::time::Instant::from_std(discovery.poll_timeout());
    let mut buf = vec![0u8; 1024];
    loop {
        match tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
            Err(_) => break,
            Ok(Ok((len, source))) => {
                if let Some(datagram) = buf.get(..len) {
                    discovery.handle_input(source, datagram, Instant::now());
                }
            }
            Ok(Err(err)) => tracing::debug!(%err, "discovery receive failed"),
        }
        if stop_after_first && !discovery.gateways().is_empty() {
            break;
        }
    }
    Ok(discovery.gateways().to_vec())
}
