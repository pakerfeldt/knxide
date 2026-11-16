//! Reading a comparison: from the bytes in which a device differs from the plan back to the
//! parameters and group address links they mean.
//!
//! A tool that has read a device's memory knows addresses and bytes. A person wants to know
//! that "Delay" is 5 in the device and 10 here, or that object 3 sends to another group
//! address. [`decode`] makes that step for parameters kept in memory and for the address and
//! association tables, and leaves everything else as bytes.
//!
//! Decoding is the inverse of what the image and the tables are built with, so it is exact
//! for whatever those rules are exact for; it has not been tried on a real device.

use std::collections::{BTreeMap, BTreeSet};

use knxide_core::GroupAddress;

use crate::config::Configuration;
use crate::device::DeviceData;
use crate::download::Download;
use crate::edit::{check_value, describe, display_value, label};
use crate::error::Error;
use crate::mask::TableSystem;
use crate::product::{ApplicationProgram, ParameterKind, ParameterLocation};
use crate::tables::SYSTEM2_ASSOCIATION_POINTER;

/// One thing in which a device differs from the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// A parameter has another value in the device.
    Parameter {
        /// Identifier of the parameter reference.
        reference: String,
        /// Its label.
        label: String,
        /// The value here.
        here: String,
        /// The value here, as it reads on a parameter page.
        here_text: String,
        /// The value in the device.
        device: String,
        /// The value in the device, as it reads.
        device_text: String,
        /// Whether the device's value is one the parameter can be given here.
        adoptable: bool,
    },
    /// A group object is linked to other group addresses in the device.
    Links {
        /// Identifier of the group object reference links are stored under here, when the
        /// object is in use here.
        reference: Option<String>,
        /// Group object number.
        number: u16,
        /// What the object is called.
        name: String,
        /// The links here; the first is the sending address.
        here: Vec<GroupAddress>,
        /// The links in the device.
        device: Vec<GroupAddress>,
    },
    /// Bytes that differ and map to no parameter and no link.
    Bytes {
        /// Address of the first byte.
        address: u16,
        /// The bytes here.
        here: Vec<u8>,
        /// The bytes in the device.
        device: Vec<u8>,
        /// What the bytes belong to, where that is known.
        what: Option<String>,
    },
}

/// A stretch of memory read from a device that differs from the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// Address of the first byte.
    pub address: u16,
    /// What the device holds from there on.
    pub device: Vec<u8>,
}

/// The device's memory as far as the comparison knows it: where it differs, what was read;
/// elsewhere, where the download sends a byte, that byte (it was read and found equal).
struct Seen<'a> {
    download: &'a Download,
    differing: BTreeMap<u32, u8>,
    /// Bytes read where the download sends nothing.
    beside: BTreeMap<u32, u8>,
}

impl<'a> Seen<'a> {
    fn new(download: &'a Download, differences: &[Difference], beside: &[Difference]) -> Self {
        let spread = |stretches: &[Difference]| {
            let mut bytes = BTreeMap::new();
            for stretch in stretches {
                for (offset, byte) in stretch.device.iter().enumerate() {
                    bytes.insert(u32::from(stretch.address) + offset as u32, *byte);
                }
            }
            bytes
        };
        Self {
            download,
            differing: spread(differences),
            beside: spread(beside),
        }
    }

    /// The byte the device holds at `address`, if the comparison read it.
    fn byte(&self, address: u32) -> Option<u8> {
        match self.differing.get(&address) {
            Some(byte) => Some(*byte),
            None if self.download.image.is_sent(address) => self.download.image.byte(address),
            None => self.beside.get(&address).copied(),
        }
    }

    fn differs(&self, from: u32, to: u32) -> bool {
        self.differing.range(from..to).next().is_some()
    }
}

/// Say what `differences` mean. `download` is the variant the comparison read against,
/// `effective` the device data it was prepared from.
pub fn decode(
    program: &ApplicationProgram,
    effective: &DeviceData,
    download: &Download,
    differences: &[Difference],
    beside: &[Difference],
) -> Result<Vec<Finding>, Error> {
    let seen = Seen::new(download, differences, beside);
    let mut explained: BTreeSet<u32> = BTreeSet::new();
    let mut findings = Vec::new();
    parameters(program, effective, &seen, &mut explained, &mut findings)?;
    let mut notes: BTreeMap<u32, String> = BTreeMap::new();
    tables(
        program,
        effective,
        &seen,
        &mut explained,
        &mut notes,
        &mut findings,
    );

    // What is left, as stretches of bytes.
    let mut last: Option<u32> = None;
    for (address, device) in &seen.differing {
        if explained.contains(address) {
            continue;
        }
        let byte = download.image.byte(*address).unwrap_or(0);
        let note = notes.get(address).cloned();
        match findings.last_mut() {
            Some(Finding::Bytes {
                here,
                device: theirs,
                what,
                ..
            }) if last.is_some_and(|last| last + 1 == *address) && *what == note => {
                here.push(byte);
                theirs.push(*device);
            }
            _ => findings.push(Finding::Bytes {
                address: *address as u16,
                here: vec![byte],
                device: vec![*device],
                what: note,
            }),
        }
        last = Some(*address);
    }
    Ok(findings)
}

/// Parameters in use whose memory holds another value in the device.
fn parameters(
    program: &ApplicationProgram,
    effective: &DeviceData,
    seen: &Seen<'_>,
    explained: &mut BTreeSet<u32>,
    findings: &mut Vec<Finding>,
) -> Result<(), Error> {
    let configuration = Configuration::evaluate(program, effective)?;
    for (index, reference) in program.parameter_refs.iter().enumerate() {
        if !configuration.parameter_active(index) {
            continue;
        }
        let Some(parameter) = program.parameter(&reference.parameter) else {
            continue;
        };
        let ParameterLocation::Memory { segment, bit } = &parameter.location else {
            continue;
        };
        let (Some(segment), Some((_, kind, ..))) =
            (program.segment(segment), describe(program, reference))
        else {
            continue;
        };
        if kind.size_in_bit == 0 || kind.size_in_bit > 64 * 8 {
            continue;
        }
        let first_bit = u64::from(segment.address) * 8 + u64::from(*bit);
        let end_bit = first_bit + u64::from(kind.size_in_bit);
        let (first, end) = ((first_bit / 8) as u32, end_bit.div_ceil(8) as u32);
        if !seen.differs(first, end) {
            continue;
        }
        // The field's bits as the device holds them.
        let mut bits = Vec::with_capacity(kind.size_in_bit as usize);
        for position in first_bit..end_bit {
            let Some(byte) = seen.byte((position / 8) as u32) else {
                bits.clear();
                break;
            };
            bits.push(byte >> (7 - position % 8) & 1 == 1);
        }
        if bits.is_empty() {
            continue;
        }
        let device = match &kind.kind {
            ParameterKind::Number { signed } if bits.len() <= 64 => {
                let raw = bits
                    .iter()
                    .fold(0u64, |value, bit| value << 1 | u64::from(*bit));
                if *signed && bits.first() == Some(&true) && bits.len() < 64 {
                    (raw as i64 - (1i64 << bits.len())).to_string()
                } else {
                    raw.to_string()
                }
            }
            ParameterKind::Restriction if bits.len() <= 64 => bits
                .iter()
                .fold(0u64, |value, bit| value << 1 | u64::from(*bit))
                .to_string(),
            ParameterKind::Text => bits
                .chunks(8)
                .map(|chunk| {
                    chunk
                        .iter()
                        .fold(0u8, |byte, bit| byte << 1 | u8::from(*bit))
                })
                .take_while(|byte| *byte != 0)
                .map(char::from)
                .collect(),
            _ => continue,
        };
        let here = configuration.value(program, index)?;
        if device.trim() == here.trim() {
            // The bytes differ in bits that belong to something else.
            continue;
        }
        explained.extend((first..end).filter(|address| seen.differing.contains_key(address)));
        findings.push(Finding::Parameter {
            reference: reference.id.clone(),
            label: label(program, reference),
            here_text: display_value(program, reference, here),
            here: here.to_string(),
            device_text: display_value(program, reference, &device),
            adoptable: check_value(program, &reference.id, &device).is_ok(),
            device,
        });
    }
    Ok(())
}

/// Group objects whose links, read from the device's address and association tables, are
/// others than here. Differences in the tables that cannot be read that way get a note.
///
/// Where the device's group address table and association table are, as far as the bytes
/// read tell: the addresses of every byte that makes them up, and of those the ones not read.
struct DeviceTables {
    /// The byte that says where the association table is, on masks that have one.
    pointer: Option<u32>,
    /// Address of the device's association table, when it is known.
    association_table: Option<u32>,
    /// Bytes of the tables that were not read.
    missing: Vec<u32>,
}

fn device_tables(seen: &Seen<'_>) -> Option<DeviceTables> {
    let tables = seen.download.tables.as_ref()?;
    // On a BCU 2 the association table follows the address table, and a byte points at it:
    // a device with another number of addresses has it elsewhere than the plan.
    let pointer = (seen.download.profile.tables == TableSystem::System2
        && (0x0100..=0x01FF).contains(&tables.association_table))
    .then_some(SYSTEM2_ASSOCIATION_POINTER);
    let mut missing = Vec::new();
    let mut read = |address: u32| {
        let byte = seen.byte(address);
        if byte.is_none() {
            missing.push(address);
        }
        byte
    };
    if let Some(count) = read(tables.address_table) {
        for at in 0..2 * u32::from(count.saturating_sub(1)) {
            read(tables.address_table + 3 + at);
        }
    }
    let association_table = match pointer {
        Some(pointer) => read(pointer).map(|low| 0x0100 | u32::from(low)),
        None => Some(tables.association_table),
    };
    if let Some(pairs) = association_table.and_then(&mut read) {
        for at in 0..2 * u32::from(pairs) {
            read(association_table.unwrap_or_default() + 1 + at);
        }
    }
    Some(DeviceTables {
        pointer,
        association_table,
        missing,
    })
}

/// What a comparison should read besides the bytes the download sends, as stretches of
/// `(address, length)`: the parts of the device's group address and association tables that
/// lie where the download sends nothing. That happens when the device holds another number
/// of addresses than the plan. `differences` and `beside` are what was read so far; ask again
/// after reading, since one table says where the next one is. Empty when nothing is missing.
#[must_use]
pub fn table_gaps(
    download: &Download,
    differences: &[Difference],
    beside: &[Difference],
) -> Vec<(u16, usize)> {
    let seen = Seen::new(download, differences, beside);
    let missing = device_tables(&seen).map_or_else(Vec::new, |tables| tables.missing);
    let mut gaps: Vec<(u16, usize)> = Vec::new();
    for address in missing {
        // Only what a memory read reaches and the image knows as memory of the device.
        let Ok(address) = u16::try_from(address) else {
            continue;
        };
        if download.image.byte(u32::from(address)).is_none() {
            continue;
        }
        match gaps.last_mut() {
            Some((start, length)) if u32::from(*start) + *length as u32 == u32::from(address) => {
                *length += 1;
            }
            _ => gaps.push((address, 1)),
        }
    }
    gaps
}

fn tables(
    program: &ApplicationProgram,
    effective: &DeviceData,
    seen: &Seen<'_>,
    explained: &mut BTreeSet<u32>,
    notes: &mut BTreeMap<u32, String>,
    findings: &mut Vec<Finding>,
) {
    let Some(tables) = &seen.download.tables else {
        return;
    };
    let address_end = tables.address_table + 3 + 2 * tables.addresses.len() as u32;
    let association_end = tables.association_table + 1 + 2 * tables.associations.len() as u32;
    let (header, entry) = match seen.download.profile.tables {
        TableSystem::System2 => (2, 3),
        TableSystem::System7 => (3, 4),
    };
    let slots = tables
        .objects
        .iter()
        .map(|object| u32::from(object.number) + 1)
        .max()
        .unwrap_or(0);
    let object_end = tables.object_table + header + entry * slots;
    let Some(DeviceTables {
        pointer,
        association_table: device_association,
        ..
    }) = device_tables(seen)
    else {
        return;
    };
    for address in seen.differing.keys() {
        if (tables.object_table + header..object_end).contains(address) {
            let number = (address - tables.object_table - header) / entry;
            notes.insert(
                *address,
                format!("group object {number}: flags or size in the object table"),
            );
        } else if Some(*address) == pointer {
            notes.insert(*address, "where the association table is".into());
        } else if (tables.address_table..address_end).contains(address) {
            notes.insert(*address, "group address table".into());
        } else if (tables.association_table..association_end).contains(address) {
            notes.insert(*address, "association table".into());
        }
    }
    if !seen.differs(tables.address_table, address_end)
        && !seen.differs(tables.association_table, association_end)
    {
        return;
    }

    // The device's tables, as far as they lie within what was read.
    let read = |address: u32| seen.byte(address);
    let Some(count) = read(tables.address_table) else {
        return;
    };
    let mut device_addresses = Vec::new();
    for at in 0..u32::from(count.saturating_sub(1)) {
        let place = tables.address_table + 3 + 2 * at;
        let (Some(high), Some(low)) = (read(place), read(place + 1)) else {
            // The device has more addresses than the plan: the rest was not read.
            return;
        };
        device_addresses.push(GroupAddress::from_raw(u16::from_be_bytes([high, low])));
    }
    let Some(device_association) = device_association else {
        return;
    };
    let Some(pairs) = read(device_association) else {
        return;
    };
    let mut device_links: BTreeMap<u16, Vec<GroupAddress>> = BTreeMap::new();
    for at in 0..u32::from(pairs) {
        let place = device_association + 1 + 2 * at;
        let (Some(connection), Some(object)) = (read(place), read(place + 1)) else {
            return;
        };
        // Connection numbers start at 1; 0xFE marks an object without address.
        let address = usize::from(connection)
            .checked_sub(1)
            .and_then(|index| device_addresses.get(index));
        if let Some(address) = address {
            let links = device_links.entry(u16::from(object)).or_default();
            if !links.contains(address) {
                links.push(*address);
            }
        }
    }

    // The reference links of each object are stored under: the one the device data links,
    // else the first.
    let reference_of = |number: u16| {
        let mut candidates = program.com_object_refs.iter().filter(|reference| {
            program
                .com_object(&reference.object)
                .is_some_and(|object| object.number == number)
        });
        let linked = candidates.clone().find(|reference| {
            effective
                .object_links
                .iter()
                .any(|entry| program.local_id(&entry.reference) == reference.id)
        });
        linked.or_else(|| candidates.next())
    };
    let mut numbers: BTreeSet<u16> = device_links.keys().copied().collect();
    numbers.extend(
        tables
            .objects
            .iter()
            .filter(|object| object.active && !object.links.is_empty())
            .map(|object| object.number),
    );
    let mut any = false;
    for number in numbers {
        let here: Vec<GroupAddress> = tables
            .objects
            .iter()
            .find(|object| object.number == number && object.active)
            .map(|object| object.links.clone())
            .unwrap_or_default();
        let device = device_links.remove(&number).unwrap_or_default();
        if here == device {
            continue;
        }
        any = true;
        let reference = reference_of(number);
        let name = reference
            .and_then(|reference| {
                reference.text.clone().or_else(|| {
                    program
                        .com_object(&reference.object)
                        .map(|object| object.text.clone())
                })
            })
            .unwrap_or_default();
        findings.push(Finding::Links {
            reference: reference.map(|reference| reference.id.clone()),
            number,
            name,
            here,
            device,
        });
    }
    if any {
        explained.extend(
            seen.differing
                .keys()
                .filter(|address| {
                    Some(**address) == pointer
                        || (tables.address_table..address_end).contains(address)
                        || (tables.association_table..association_end).contains(address)
                })
                .copied(),
        );
    }
}
