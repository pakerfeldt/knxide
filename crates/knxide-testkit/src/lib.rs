//! Test support for the knxide crates: a fake KNXnet/IP gateway built on `knxide-ip`, frame
//! fixtures and helpers. Development use only.

pub mod blocking;
pub mod gateway;

pub use blocking::BlockingTunnel;
pub use gateway::{Behaviour, FakeGateway, GatewayCore};
