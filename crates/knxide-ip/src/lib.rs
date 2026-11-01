//! KNXnet/IP frame types and sans-IO protocol state machines (discovery, tunnelling).
//!
//! The state machines never touch sockets or clocks: callers feed in datagrams and the current
//! time, then drain outgoing datagrams, timer deadlines and events.

// Decoders must never panic on malformed input (brief §9.4).
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod discovery;
pub mod frame;
pub mod router;
pub mod server;
pub mod structures;
pub mod tunnel;

pub use frame::{Frame, Status};
pub use structures::{Crd, Cri, DeviceInfo, Dib, Dibs, Hpai, ServiceFamily};
