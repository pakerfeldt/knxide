//! Where edits live: a file beside the project, never the project itself.
//!
//! The file is JSON, named `<project file name>.knxide.json` unless `--state` says otherwise:
//!
//! ```json
//! {
//!   "version": 1,
//!   "project": "P-0001",
//!   "devices": {
//!     "P-0001-0_DI-1": {
//!       "parameters": { "P-9_R-9": "1" },
//!       "links": { "O-0_R-1": ["1/1/3", "1/2/2"] },
//!       "downloaded": {
//!         "time_ms": 1763222400000,
//!         "digest": "9f2c41d07a3b55e1",
//!         "configuration": { "parameters": { "P-9_R-9": "1" }, "links": {} }
//!       }
//!     }
//!   },
//!   "group_addresses": {
//!     "created": [{ "address": "1/1/9", "name": "Hall night light", "datapoint_type": "DPST-1-1" }],
//!     "renamed": { "1/1/1": "Hall ceiling light" }
//!   },
//!   "floor_plans": [
//!     { "id": "plan-1", "name": "Ground floor", "image": "plan-1.png",
//!       "positions": { "P-0001-0_DI-1": { "x": 4250, "y": 6100 } } }
//!   ]
//! }
//! ```
//!
//! A floor plan is an image the user supplies, kept as a file in the directory
//! `<state file>.plans` beside the state file, and the places the user gave devices on it:
//! hundredths of a percent of the image's width and height, so that they hold at any size.
//!
//! `parameters` and `links` are the device as it is edited now: what differs from the
//! project. `downloaded` is what the app last programmed into it: when, a digest of the memory
//! image, and under `configuration` the edits as they were at that moment. That configuration
//! is the device's baseline: "changed" means different from it, not from the project. A file
//! written before the configuration was recorded has only the digest; see
//! [`Downloaded::edits`]. `"interrupted": true` marks a device whose download stopped after
//! writing had begun: it needs programming whatever its values are.
//!
//! Parameters are stored by parameter reference identifier, links by group object reference
//! identifier with the sending address first. It is written whole, to a temporary file that
//! then takes the place of the old one, so a crash leaves either the old state or the new.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use knxide_core::GroupAddress;
use knxide_program::{Download, Edits};
use serde::{Deserialize, Serialize};

/// The version of the file format this code writes.
const VERSION: u32 = 1;

/// What the device is known to hold: the record of the last download the app made to it, or
/// of a comparison that found it the same as the configuration here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// When it was programmed, in milliseconds since the Unix epoch; `None` when the device
    /// was only found to hold the configuration.
    pub time_ms: Option<u64>,
    /// A digest of the memory image; see [`digest`].
    pub digest: String,
    /// The configuration itself: the edits over the project as they were then. `None` in a
    /// record written before this was kept; then only the digest can say whether the
    /// configuration here is the one in the device.
    pub edits: Option<Edits>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct EditsFile {
    #[serde(default)]
    parameters: BTreeMap<String, String>,
    #[serde(default)]
    links: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DownloadedFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    time_ms: Option<u64>,
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    configuration: Option<EditsFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct DeviceFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    parameters: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    links: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    downloaded: Option<DownloadedFile>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    interrupted: bool,
}

fn links_to_file(edits: &Edits) -> BTreeMap<String, Vec<String>> {
    edits
        .links
        .iter()
        .map(|(reference, links)| {
            (
                reference.clone(),
                links.iter().map(ToString::to_string).collect(),
            )
        })
        .collect()
}

/// A group address made in the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedAddress {
    /// The address, as `main/middle/sub`.
    pub address: String,
    /// Its name.
    pub name: String,
    /// Its datapoint type, for example `DPST-1-1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datapoint_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct AddressesFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    created: Vec<CreatedAddress>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    renamed: BTreeMap<String, String>,
}

/// Where a device stands on a floor plan: hundredths of a percent of the image's width and
/// height, from its top left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// Across, 0 to 10000.
    pub x: u16,
    /// Down, 0 to 10000.
    pub y: u16,
}

/// A floor plan: an image and the devices placed on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorPlan {
    /// Identifier, `plan-1`.
    pub id: String,
    /// What the user calls it.
    pub name: String,
    /// File name of the image in the plans directory.
    pub image: String,
    /// Placed devices, by device identifier.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub positions: BTreeMap<String, Position>,
}

/// The directory the images of the floor plans are kept in for the state file at `state`.
pub fn plans_directory(state: &Path) -> PathBuf {
    let mut name = state.as_os_str().to_os_string();
    name.push(".plans");
    PathBuf::from(name)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct File {
    version: u32,
    project: String,
    #[serde(default)]
    devices: BTreeMap<String, DeviceFile>,
    #[serde(default)]
    group_addresses: AddressesFile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    floor_plans: Vec<FloorPlan>,
}

/// What the app keeps for one device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceState {
    /// What is different from the project.
    pub edits: Edits,
    /// What the device is known to hold.
    pub downloaded: Option<Downloaded>,
    /// A download stopped after writing had begun, and none has succeeded since.
    pub interrupted: bool,
}

/// Everything the app keeps beside the project.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// Per device identifier.
    pub devices: BTreeMap<String, DeviceState>,
    /// Group addresses made in the app, by address.
    pub created: BTreeMap<GroupAddress, CreatedAddress>,
    /// Names given in the app to group addresses of the project, by address.
    pub renamed: BTreeMap<GroupAddress, String>,
    /// The floor plans, in the order they were made.
    pub plans: Vec<FloorPlan>,
}

/// The file the state is kept in for the project at `project`: beside it, with
/// `.knxide.json` behind its name.
pub fn default_path(project: &Path) -> PathBuf {
    let mut name = project
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".knxide.json");
    project.with_file_name(name)
}

impl State {
    /// Read the state for the project `project` from `path`. A file that is not there is an
    /// empty state. A file for another project, or one that cannot be read, is an error:
    /// nothing is overwritten on a guess.
    pub fn load(path: &Path, project: &str) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let file: File =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        if file.version != VERSION {
            return Err(format!(
                "{}: written by another version of this program (format {})",
                path.display(),
                file.version
            ));
        }
        if file.project != project {
            return Err(format!(
                "{} holds the edits of project {}, not of {project}",
                path.display(),
                file.project
            ));
        }
        let address = |text: &str| {
            text.parse::<GroupAddress>()
                .map_err(|_| format!("{}: {text:?} is not a group address", path.display()))
        };
        let mut state = Self::default();
        let edits_of = |parameters: BTreeMap<String, String>,
                        links: BTreeMap<String, Vec<String>>|
         -> Result<Edits, String> {
            let mut edits = Edits {
                parameters,
                ..Edits::default()
            };
            for (reference, links) in links {
                let links = links
                    .iter()
                    .map(|link| address(link))
                    .collect::<Result<_, _>>()?;
                edits.links.insert(reference, links);
            }
            Ok(edits)
        };
        for (id, device) in file.devices {
            let downloaded = match device.downloaded {
                Some(record) => Some(Downloaded {
                    time_ms: record.time_ms,
                    digest: record.digest,
                    edits: match record.configuration {
                        Some(kept) => Some(edits_of(kept.parameters, kept.links)?),
                        None => None,
                    },
                }),
                None => None,
            };
            state.devices.insert(
                id,
                DeviceState {
                    edits: edits_of(device.parameters, device.links)?,
                    downloaded,
                    interrupted: device.interrupted,
                },
            );
        }
        for created in file.group_addresses.created {
            state.created.insert(address(&created.address)?, created);
        }
        for (text, name) in file.group_addresses.renamed {
            state.renamed.insert(address(&text)?, name);
        }
        state.plans = file.floor_plans;
        Ok(state)
    }

    /// Write the state to `path`, in one step: a temporary file beside it is written and
    /// renamed over it.
    pub fn save(&self, path: &Path, project: &str) -> Result<(), String> {
        let file = File {
            version: VERSION,
            project: project.to_string(),
            devices: self
                .devices
                .iter()
                .filter(|(_, device)| {
                    !device.edits.is_empty() || device.downloaded.is_some() || device.interrupted
                })
                .map(|(id, device)| {
                    (
                        id.clone(),
                        DeviceFile {
                            parameters: device.edits.parameters.clone(),
                            links: links_to_file(&device.edits),
                            downloaded: device.downloaded.as_ref().map(|record| DownloadedFile {
                                time_ms: record.time_ms,
                                digest: record.digest.clone(),
                                configuration: record.edits.as_ref().map(|edits| EditsFile {
                                    parameters: edits.parameters.clone(),
                                    links: links_to_file(edits),
                                }),
                            }),
                            interrupted: device.interrupted,
                        },
                    )
                })
                .collect(),
            group_addresses: AddressesFile {
                created: self.created.values().cloned().collect(),
                renamed: self
                    .renamed
                    .iter()
                    .map(|(address, name)| (address.to_string(), name.clone()))
                    .collect(),
            },
            floor_plans: self.plans.clone(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|error| error.to_string())?;
        let mut temporary = path.as_os_str().to_os_string();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        let failed = |error: std::io::Error| format!("{}: {error}", path.display());
        std::fs::write(&temporary, text + "\n").map_err(failed)?;
        std::fs::rename(&temporary, path).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            failed(error)
        })
    }
}

/// A digest of what a download writes: every segment of its image with the bytes and which of
/// them are sent. Two configurations with the same digest lead to the same device memory.
/// FNV-1a, 64 bits; this is bookkeeping, not security.
pub fn digest(download: &Download) -> String {
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    let mut feed = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    };
    // The full variant where there is one: it covers every byte either variant sends.
    let image = &download.fallback.as_deref().unwrap_or(download).image;
    for segment in image.segments() {
        segment
            .address
            .to_be_bytes()
            .into_iter()
            .for_each(&mut feed);
        for (byte, send) in segment.data.iter().zip(&segment.send) {
            feed(*byte);
            feed(u8::from(*send));
        }
    }
    format!("{hash:016x}")
}
