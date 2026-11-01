//! KNX over KNXnet/IP for Rust.
//!
//! This crate gathers the knxide crates in one place. Use the individual crates instead if
//! you only need part of it, for example `knxide-core` on an embedded target.
//!
//! ```no_run
//! use std::time::Duration;
//! use knxide::{Client, TunnelOptions, dpt, ga};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::connect("192.168.1.50:3671".parse()?, TunnelOptions::default()).await?;
//! client.group_write(ga!(1/2/3), dpt::dpt1::encode(true)).await?;
//! let raw = client.group_read(ga!(1/2/4), Duration::from_secs(2)).await?;
//! println!("temperature: {}", dpt::dpt9::decode(&raw)?);
//! # Ok(()) }
//! ```
//!
//! Features (both on by default): `tokio` for the async client, `project` for ETS imports.

/// Addresses, telegrams and datapoint types.
pub use knxide_core as core;
pub use knxide_core::{GroupAddress, IndividualAddress, dpt, ga};
/// KNXnet/IP frames and the I/O-free connection state machines.
pub use knxide_ip as ip;
/// Group address import from ETS exports.
#[cfg(feature = "project")]
pub use knxide_project as project;
/// The async tokio client.
#[cfg(feature = "tokio")]
pub use knxide_tokio as client;
#[cfg(feature = "tokio")]
pub use knxide_tokio::{Client, Error, TunnelOptions};
