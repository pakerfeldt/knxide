//! Errors from the async client.

use knxide_core::dpt::DptError;
use knxide_ip::tunnel::{DisconnectReason, SendError};

/// An error from [`crate::Client`] or [`crate::discover`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Socket error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The first connection attempt failed.
    #[error("could not connect: {0}")]
    Connect(DisconnectReason),
    /// A frame could not be sent.
    #[error("{0}")]
    Send(#[from] SendError),
    /// A value could not be encoded or decoded.
    #[error("{0}")]
    Dpt(#[from] DptError),
    /// No answer in time.
    #[error("timed out")]
    Timeout,
    /// The client is closed.
    #[error("client is closed")]
    Closed,
    /// No device acknowledged frames sent to this individual address.
    #[error("no device answers at {0}")]
    DeviceNotResponding(knxide_core::IndividualAddress),
    /// Not supported, e.g. IPv6.
    #[error("{0} is not supported")]
    Unsupported(&'static str),
}
