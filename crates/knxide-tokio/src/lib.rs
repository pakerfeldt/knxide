//! Async adapter for KNXnet/IP: owns the sockets and timers and drives the `knxide-ip` state
//! machines from a single driver task.
//!
//! ```no_run
//! use std::time::Duration;
//! use knxide_core::{dpt, ga};
//! use knxide_tokio::{Client, TunnelOptions};
//!
//! # async fn example() -> Result<(), knxide_tokio::Error> {
//! let client = Client::connect("192.168.1.50:3671".parse().unwrap(), TunnelOptions::default()).await?;
//! client.group_write(ga!(1/2/3), dpt::dpt1::encode(true)).await?;
//! let raw = client.group_read(ga!(1/2/4), Duration::from_secs(2)).await?;
//! println!("temperature: {}", dpt::dpt9::decode(&raw)?);
//! # Ok(()) }
//! ```

mod client;
mod discover;
mod error;
mod link;
mod management;
mod routing;

pub use client::{Client, ClientEvent, ConnectionState, TunnelOptions};
pub use discover::{describe, discover};
pub use error::Error;
pub use knxide_ip::discovery::Gateway;
pub use routing::RoutingOptions;
