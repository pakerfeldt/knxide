//! Work out what an application download to a KNX device consists of: the memory image
//! (parameters and communication tables) and the ordered list of bus requests.
//!
//! Nothing here opens a connection. Parsing takes text or bytes, planning runs against anything
//! that implements [`Device`], and [`SimulatedDevice`] stands in for a device by replaying a
//! recording or by modelling a well-behaved one. For a real bus, [`wire`] turns requests into
//! application layer PDUs and [`transport`] is the transport layer connection as a state
//! machine; sockets and timers are the caller's.
//!
//! # Scope
//!
//! "Download application" for one device whose individual address is already programmed, for
//! mask versions `0021` (BCU 2), `0701` and `0705` (BIM M112), with the product's own load
//! procedure, at APDU length 15, without KNX Secure and without manufacturer plug-ins.
//!
//! The rules come from behaviour notes and from three recorded downloads, one per mask, which
//! this crate reproduces request for request with nothing but the device's answers taken from
//! the recording. Each item's documentation says whether it is verified against a recorded
//! download or not. Unverified parts are best guesses and must not be trusted with a real
//! device unchecked.
//!
//! # Known gaps
//!
//! - **What the recordings do not show.** Every check of the pre-check passes in them, the
//!   one partial download finds its device loaded, and no frame is lost. What happens
//!   otherwise (a check that stops the run, the full download a partial one falls back to,
//!   repetitions and timeouts) follows behaviour notes.
//! - **Compatible manufacturers.** The recorded tool accepts a device of another manufacturer
//!   when its data lists the two as compatible. This crate has no such list and accepts only
//!   the same manufacturer.
//! - **Property sizes.** A step that writes or compares a property needs the size of its
//!   elements. [`MaskProfile::property_element_size`] knows a handful; others stop the plan.
//! - **Differential downloads.** The checksum comparison before a partial download is made, but
//!   a match does not shrink the download.
//! - **Parameters kept in properties** are not downloaded; the one use this crate makes of
//!   them is the hardware comparison of the pre-check.
//! - Devices with a second application program, objects with several group addresses, text
//!   and signed parameters, and anything else a recording does not exercise, are implemented
//!   from notes alone or refused.
//!
//! # Stages
//!
//! 1. [`ApplicationProgram::parse`] reads the product's application program XML;
//!    [`DeviceData::parse`] and [`GroupAddresses`] read what the project stores for the device.
//!    [`Project`] does both for every device of a project file.
//! 2. [`Download::prepare`] evaluates values and visibility ([`Configuration`]), builds the
//!    memory [`Image`] and the communication [`Tables`], and rewrites the load procedure.
//! 3. [`Download::plan`] runs the opening, the pre-check and the procedure against a
//!    [`Device`] and returns the [`Plan`].
//!
//! ```
//! use knxide_program::{
//!     ApplicationProgram, DeviceData, Download, GroupAddresses, PlanOptions, SimulatedDevice,
//! };
//!
//! // A made-up product: one segment with a one-byte parameter, no tables.
//! let product = r#"
//! <ApplicationProgram Id="M-00FA_A-0001-01-0000" ApplicationNumber="1" ApplicationVersion="1"
//!     MaskVersion="MV-0701" LoadProcedureStyle="ProductProcedure">
//!   <Static>
//!     <Code>
//!       <AbsoluteSegment Id="M-00FA_A-0001-01-0000_AS-4400" Address="17408" Size="2">
//!         <Data>AAA=</Data>
//!       </AbsoluteSegment>
//!     </Code>
//!     <ParameterTypes>
//!       <ParameterType Id="M-00FA_A-0001-01-0000_PT-Byte">
//!         <TypeNumber SizeInBit="8" Type="unsignedInt" />
//!       </ParameterType>
//!     </ParameterTypes>
//!     <Parameters>
//!       <Parameter Id="M-00FA_A-0001-01-0000_P-1" ParameterType="M-00FA_A-0001-01-0000_PT-Byte"
//!           Value="7">
//!         <Memory CodeSegment="M-00FA_A-0001-01-0000_AS-4400" Offset="1" BitOffset="0" />
//!       </Parameter>
//!     </Parameters>
//!     <ParameterRefs>
//!       <ParameterRef Id="M-00FA_A-0001-01-0000_P-1_R-1" RefId="M-00FA_A-0001-01-0000_P-1" />
//!     </ParameterRefs>
//!     <LoadProcedures>
//!       <LoadProcedure>
//!         <LdCtrlLoad LsmIdx="3" />
//!         <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="17408" Size="2" Access="255"
//!             MemType="3" SegFlags="128" />
//!         <LdCtrlLoadCompleted LsmIdx="3" />
//!       </LoadProcedure>
//!     </LoadProcedures>
//!   </Static>
//!   <Dynamic>
//!     <ParameterRefRef RefId="M-00FA_A-0001-01-0000_P-1_R-1" />
//!   </Dynamic>
//! </ApplicationProgram>"#;
//! let device = r#"<DeviceInstance Address="5">
//!   <ParameterInstanceRefs>
//!     <ParameterInstanceRef RefId="M-00FA_A-0001-01-0000_P-1_R-1" Value="9" />
//!   </ParameterInstanceRefs>
//! </DeviceInstance>"#;
//!
//! let program = ApplicationProgram::parse(product)?;
//! let device = DeviceData::parse(device)?;
//! let download = Download::prepare(&program, &device, &GroupAddresses::new())?;
//! assert_eq!(download.image.byte(0x4401), Some(9));
//!
//! let plan = download.plan(&mut SimulatedDevice::holding(&download), &PlanOptions::default())?;
//! let lines: Vec<String> = plan.requests.iter().map(ToString::to_string).collect();
//! assert_eq!(lines[0], "T_CONNECT");
//! // The model's memory reads as zero, so only the changed byte is written.
//! assert!(lines.contains(&"MEM_WRITE 4401 09".to_string()));
//! assert_eq!(lines.last().unwrap(), "T_DISCONNECT");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

// Readers must never panic on malformed input.
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

mod base64;
pub mod compare;
pub mod config;
pub mod device;
pub mod download;
pub mod edit;
pub mod error;
pub mod image;
pub mod mask;
pub mod plan;
pub mod procedure;
pub mod product;
pub mod project;
pub mod request;
pub mod sim;
pub mod tables;
pub mod transport;
pub mod view;
pub mod wire;
mod xml;
mod zip;

pub use config::Configuration;
pub use device::{DeviceData, DownloadStamp, GroupAddresses};
pub use download::{ApplicationId, Download, HardwareExpectation};
pub use edit::{Changes, EditError, Edits};
pub use error::Error;
pub use image::Image;
pub use mask::{MaskProfile, Resource};
pub use plan::{Device, Expect, Fault, Plan, PlanError, PlanFailure, PlanOptions, Progress};
pub use procedure::{DownloadKind, LoadStep};
pub use product::ApplicationProgram;
pub use project::{Obstacle, Project, ProjectArchive, ProjectDevice, ProjectSource};
pub use request::{Request, Response};
pub use sim::SimulatedDevice;
pub use tables::Tables;
pub use view::{DeviceView, view};
