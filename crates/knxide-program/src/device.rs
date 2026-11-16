//! What a project stores for one device: changed parameter values, group address links and
//! the record of its last download.

use std::collections::HashMap;

use knxide_core::GroupAddress;

use crate::base64;
use crate::error::{Error, invalid};
use crate::product::{ObjectAttributes, parse_bool};
use crate::xml::{self, Element};

/// A parameter value the user changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterValue {
    /// Identifier of the parameter reference, with or without the application program's
    /// identifier in front.
    pub reference: String,
    /// The value.
    pub value: String,
}

/// The project's entry for one group object reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectLinks {
    /// Identifier of the group object reference, with or without the application program's
    /// identifier in front.
    pub reference: String,
    /// Identifiers of the linked group addresses (`GA-<n>`); the first is the sending one.
    pub links: Vec<String>,
    /// Attributes changed for this device. Unverified: the recorded projects change none.
    pub overrides: ObjectAttributes,
}

/// Checksums of the device's memory blocks as the tool recorded them after its last download
/// (project attribute `CheckSums`). Compared with the device before a partial download.
///
/// The layout was worked out from project files: a marker, the stamp type, the number of
/// objects, then per object its index, the number of blocks and the blocks. A block is a flag
/// and a 16-bit checksum, followed by two zero bytes in projects last written before 2016 and
/// by nothing in later ones. Both forms occur side by side in one project file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DownloadStamp {
    /// The stamped interface objects.
    pub objects: Vec<StampedObject>,
}

/// The stamp of one interface object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampedObject {
    /// Object index.
    pub object: u8,
    /// One entry per memory control block, in the order of the object's block table.
    pub blocks: Vec<StampedBlock>,
}

/// The stamp of one memory control block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StampedBlock {
    /// Whether the device protects the block with a checksum.
    pub protected: bool,
    /// The 16-bit checksum.
    pub checksum: u16,
}

impl DownloadStamp {
    /// Decode the base64 text of a `CheckSums` attribute.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let bytes = base64::decode(text)?;
        // The short block form first; a stamp in the long form does not come out even with it.
        match Self::decode(&bytes, 0) {
            Ok(Some(stamp)) => Ok(stamp),
            Ok(None) => match Self::decode(&bytes, 2)? {
                Some(stamp) => Ok(stamp),
                None => Err(invalid("download stamp", text)),
            },
            Err(error) => Err(error),
        }
    }

    /// Decode with `padding` bytes after each block's checksum. `Ok(None)` when the bytes do
    /// not fit this form.
    fn decode(bytes: &[u8], padding: usize) -> Result<Option<Self>, Error> {
        let mut reader = Reader { bytes };
        // Format marker, then the stamp type; only type 1 (block checksums) is understood.
        if reader.u32() != Some(0x0001_0001) {
            return Ok(None);
        }
        let Some(kind) = reader.u32() else {
            return Ok(None);
        };
        if kind != 1 {
            return Err(Error::Unsupported(format!("download stamp type {kind}")));
        }
        let Some(count) = reader.u32() else {
            return Ok(None);
        };
        let mut objects = Vec::new();
        for _ in 0..count {
            let (Some(object), Some(blocks)) = (reader.u8(), reader.u32()) else {
                return Ok(None);
            };
            let mut list = Vec::new();
            for _ in 0..blocks {
                let (Some(protected), Some(low), Some(high)) =
                    (reader.u8(), reader.u8(), reader.u8())
                else {
                    return Ok(None);
                };
                for _ in 0..padding {
                    if reader.u8() != Some(0) {
                        return Ok(None);
                    }
                }
                list.push(StampedBlock {
                    protected: protected != 0,
                    checksum: u16::from_le_bytes([low, high]),
                });
            }
            objects.push(StampedObject {
                object,
                blocks: list,
            });
        }
        Ok(reader.bytes.is_empty().then_some(Self { objects }))
    }
}

/// Little-endian cursor over the stamp bytes.
struct Reader<'b> {
    bytes: &'b [u8],
}

impl Reader<'_> {
    fn u8(&mut self) -> Option<u8> {
        let (first, rest) = self.bytes.split_first()?;
        self.bytes = rest;
        Some(*first)
    }

    fn u32(&mut self) -> Option<u32> {
        let (head, rest) = self.bytes.split_first_chunk::<4>()?;
        self.bytes = rest;
        Some(u32::from_le_bytes(*head))
    }
}

/// One device of a project (element `DeviceInstance`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceData {
    /// The device part of the individual address (attribute `Address`), if present. Area and
    /// line live elsewhere in the project.
    pub address: Option<u16>,
    /// Whether the project believes the application is in the device
    /// (`ApplicationProgramLoaded`). Informational: the kind of download is decided by asking
    /// the device, not by this flag.
    pub application_loaded: bool,
    /// Values the user changed. Everything else keeps the product default.
    pub parameter_values: Vec<ParameterValue>,
    /// Group address links.
    pub object_links: Vec<ObjectLinks>,
    /// Checksums recorded after the last download, if the project has them.
    pub stamp: Option<DownloadStamp>,
    /// Whether the project also holds the image of the last download (`LoadedImage`). The image
    /// itself is not read: this crate does no differential download.
    pub has_loaded_image: bool,
}

impl DeviceData {
    /// Parse the first `DeviceInstance` element of `text`: either a whole project file or the
    /// element on its own.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let root = xml::parse(text)?;
        let device = root
            .find("DeviceInstance")
            .ok_or_else(|| Error::Missing("DeviceInstance element".into()))?;
        Self::from_element(device)
    }

    pub(crate) fn from_element(device: &Element<'_>) -> Result<Self, Error> {
        let address = device
            .attr("Address")
            .map(|text| text.trim().parse().map_err(|_| invalid("Address", text)))
            .transpose()?;
        let application_loaded = device
            .attr("ApplicationProgramLoaded")
            .map(|text| parse_bool(text, "ApplicationProgramLoaded"))
            .transpose()?
            .unwrap_or(false);
        let mut parameter_values = Vec::new();
        for element in device
            .child("ParameterInstanceRefs")
            .into_iter()
            .flat_map(|list| list.children_named("ParameterInstanceRef"))
        {
            // An entry without a value only records other state of the reference.
            if let (Some(reference), Some(value)) = (element.attr("RefId"), element.attr("Value")) {
                parameter_values.push(ParameterValue {
                    reference: reference.to_string(),
                    value: value.to_string(),
                });
            }
        }
        let mut object_links = Vec::new();
        for element in device
            .child("ComObjectInstanceRefs")
            .into_iter()
            .flat_map(|list| list.children_named("ComObjectInstanceRef"))
        {
            let Some(reference) = element.attr("RefId") else {
                continue;
            };
            object_links.push(ObjectLinks {
                reference: reference.to_string(),
                links: element
                    .attr("Links")
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
                overrides: ObjectAttributes::parse(element)?,
            });
        }
        let stamp = device
            .attr("CheckSums")
            .filter(|text| !text.trim().is_empty())
            .map(DownloadStamp::parse)
            .transpose()?;
        Ok(Self {
            address,
            application_loaded,
            parameter_values,
            object_links,
            stamp,
            has_loaded_image: device
                .attr("LoadedImage")
                .is_some_and(|text| !text.trim().is_empty()),
        })
    }
}

/// The group addresses of a project by identifier (`GA-<n>`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GroupAddresses {
    by_id: HashMap<String, GroupAddress>,
}

/// `GA-7` from `P-0123-0_GA-7`; identifiers without a prefix stay as they are.
fn short_id(id: &str) -> &str {
    id.rfind("GA-")
        .and_then(|at| id.get(at..))
        .unwrap_or(id)
        .trim()
}

impl GroupAddresses {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace the address called `id`.
    pub fn insert(&mut self, id: &str, address: GroupAddress) {
        self.by_id.insert(short_id(id).to_string(), address);
    }

    /// The address called `id`; a project prefix in front of `GA-<n>` is ignored.
    pub fn get(&self, id: &str) -> Option<GroupAddress> {
        self.by_id.get(short_id(id)).copied()
    }

    /// Number of addresses.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether there are no addresses.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Parse lines of `GA-<n> <address>`, the address as a 16-bit number. Empty lines and lines
    /// starting with `#` are skipped.
    pub fn parse_list(text: &str) -> Result<Self, Error> {
        let mut out = Self::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = || invalid("group address line", line);
            let (id, address) = line.split_once(char::is_whitespace).ok_or_else(bad)?;
            let address: u16 = address.trim().parse().map_err(|_| bad())?;
            out.insert(id, GroupAddress::from_raw(address));
        }
        Ok(out)
    }

    /// Collect every `GroupAddress` element (attributes `Id`, `Address`) of a project file.
    pub fn parse_project(text: &str) -> Result<Self, Error> {
        let root = xml::parse(text)?;
        let mut out = Self::new();
        let mut pending = vec![&root];
        while let Some(element) = pending.pop() {
            if element.name == "GroupAddress"
                && let (Some(id), Some(address)) = (element.attr("Id"), element.attr("Address"))
            {
                let address: u16 = address
                    .trim()
                    .parse()
                    .map_err(|_| invalid("group address", address))?;
                out.insert(id, GroupAddress::from_raw(address));
            }
            pending.extend(element.children.iter());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_instance() {
        let device = DeviceData::parse(
            r#"<Project><DeviceInstance Id="P-1-0_DI-1" Address="7" ApplicationProgramLoaded="true">
                 <ParameterInstanceRefs>
                   <ParameterInstanceRef RefId="M-1_A-2_P-1_R-1" Value="3" />
                   <ParameterInstanceRef RefId="P-2_R-2" />
                 </ParameterInstanceRefs>
                 <ComObjectInstanceRefs>
                   <ComObjectInstanceRef RefId="O-0_R-1" Links="GA-2 GA-1" TransmitFlag="Disabled" />
                   <ComObjectInstanceRef RefId="O-1_R-2" />
                 </ComObjectInstanceRefs>
               </DeviceInstance></Project>"#,
        )
        .unwrap();
        assert_eq!(device.address, Some(7));
        assert!(device.application_loaded);
        assert_eq!(device.parameter_values.len(), 1);
        assert_eq!(device.parameter_values[0].value, "3");
        assert_eq!(device.object_links[0].links, ["GA-2", "GA-1"]);
        assert_eq!(device.object_links[0].overrides.transmit, Some(false));
        assert!(device.object_links[1].links.is_empty());
        assert_eq!(device.stamp, None);
        assert!(!device.has_loaded_image);
        assert!(DeviceData::parse("<Project/>").is_err());
    }

    #[test]
    fn group_address_sources() {
        let list = GroupAddresses::parse_list("# made up\nGA-1 2305\n\nGA-20 4000\n").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.get("P-0001-0_GA-20").unwrap().raw(), 4000);
        assert_eq!(list.get("GA-2"), None);
        assert!(GroupAddresses::parse_list("GA-1 70000").is_err());
        assert!(GroupAddresses::parse_list("GA-1").is_err());

        let project = GroupAddresses::parse_project(
            r#"<P><GroupRanges><GroupRange><GroupAddress Id="P-0001-0_GA-3" Address="2051" Name="x"/>
               </GroupRange></GroupRanges></P>"#,
        )
        .unwrap();
        assert_eq!(project.get("GA-3").unwrap().raw(), 2051);
    }

    #[test]
    fn stamp_layout() {
        // Marker, type 1, one object (index 3) with two blocks: (protected, 0x1234) and
        // (unprotected, 0xBEEF). Encoded by hand.
        let bytes: Vec<u8> = [
            &[0x01, 0x00, 0x01, 0x00][..],
            &[0x01, 0x00, 0x00, 0x00],
            &[0x01, 0x00, 0x00, 0x00],
            &[0x03],
            &[0x02, 0x00, 0x00, 0x00],
            &[0x01, 0x34, 0x12, 0x00, 0x00],
            &[0x00, 0xEF, 0xBE, 0x00, 0x00],
        ]
        .concat();
        assert_eq!(bytes.len(), 27);
        let text = "AQABAAEAAAABAAAAAwIAAAABNBIAAADvvgAA";
        let stamp = DownloadStamp::parse(text).unwrap();
        assert_eq!(base64::decode(text).unwrap(), bytes);
        assert_eq!(
            stamp.objects,
            [StampedObject {
                object: 3,
                blocks: vec![
                    StampedBlock {
                        protected: true,
                        checksum: 0x1234
                    },
                    StampedBlock {
                        protected: false,
                        checksum: 0xBEEF
                    },
                ],
            }]
        );
        // The short form of later projects: no padding after the checksum.
        let short = DownloadStamp::parse("AQABAAEAAAABAAAAAwIAAAABNBIA774=").unwrap();
        assert_eq!(short, stamp);
        // Two objects in the short form: object 3 with one block, object 4 with two.
        let two = DownloadStamp::parse("AQABAAEAAAACAAAAAwEAAAABEREEAgAAAAEiIgAAAA==").unwrap();
        assert_eq!(two.objects.len(), 2);
        assert_eq!(two.objects[1].object, 4);
        assert_eq!(
            two.objects[1].blocks,
            [
                StampedBlock {
                    protected: true,
                    checksum: 0x2222
                },
                StampedBlock {
                    protected: false,
                    checksum: 0
                }
            ]
        );
        // Truncated, trailing bytes, wrong marker.
        assert!(DownloadStamp::parse("AQABAAEAAAABAAAAAwIAAAABNBIA").is_err());
        assert!(DownloadStamp::parse("AQABAAEAAAABAAAAAwIAAAABNBIAAADvvgAAAA==").is_err());
        assert!(DownloadStamp::parse("AAAAAAEAAAAAAAAA").is_err());
    }
}
