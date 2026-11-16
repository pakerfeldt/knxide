//! Test support for the knxide crates: a fake KNXnet/IP gateway built on `knxide-ip`, frame
//! fixtures, a made-up project and helpers. Development use only.

pub mod blocking;
pub mod gateway;
pub mod project;
pub mod zip;

pub use blocking::BlockingTunnel;
pub use gateway::{Behaviour, DeviceHandler, FakeGateway, GatewayCore};
