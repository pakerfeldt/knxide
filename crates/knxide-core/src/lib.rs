//! KNX addressing, cEMI `L_Data` frames, group services (TPCI/APCI) and datapoint type codecs.
//!
//! Pure data types and codecs: no I/O, no async, no global state. `no_std`, so it can be
//! reused on embedded targets.

#![no_std]
// Decoders must never panic on malformed input (brief §9.4).
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

extern crate alloc;

pub mod address;
pub mod apdu;
pub mod cemi;
pub mod dpt;
pub mod error;
#[doc(hidden)]
pub mod reader;

pub use address::{Destination, GroupAddress, GroupAddressStyle, IndividualAddress};
pub use apdu::{Apdu, Payload, Tpdu};
pub use cemi::{Cemi, LData, Priority};
pub use error::{DecodeError, EncodeError};
