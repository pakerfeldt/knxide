//! The three communication tables: group addresses, associations, group objects.
//!
//! Common rules, verified against three recorded downloads in which every linked group object
//! has exactly one group address:
//!
//! - A group object is *in use* when one of its references is reached in the visibility tree,
//!   and *linked* when it is in use and the project gives it at least one group address.
//! - The address table lists every group address of a linked object once, ascending by 16-bit
//!   value: `[count + 1] [individual address, 2 bytes, not written] [addresses, 2 bytes each]`.
//!   The connection number of an address is its position plus one.
//! - Per group object the object table holds a value pointer (kept from the product image), a
//!   configuration byte and a type byte.
//! - The space the product reserves for the address and association tables is not sent, apart
//!   from the bytes the tables occupy.
//!
//! Objects with several group addresses follow behaviour notes and are unverified.

use knxide_core::GroupAddress;

use crate::config::Configuration;
use crate::device::{DeviceData, GroupAddresses};
use crate::error::Error;
use crate::image::Image;
use crate::mask::{MaskProfile, TableSystem};
use crate::procedure::{Action, LoadStep, TablePlacement, Target};
use crate::product::{ApplicationProgram, ObjectAttributes, ObjectPriority, TableLocation};

/// Address of the BCU 2 byte that holds the low byte of the association table's address.
pub(crate) const SYSTEM2_ASSOCIATION_POINTER: u32 = 0x0111;

/// One group object as the tables see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupObject {
    /// Group object number.
    pub number: u16,
    /// Whether a reference to the object is in use.
    pub active: bool,
    /// Attributes: from the reference in use where it changes them, else from the object.
    pub attributes: ObjectAttributes,
    /// Group addresses in project order; the first is the sending address.
    pub links: Vec<GroupAddress>,
}

/// One entry of the association table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Association {
    /// Connection number: position in the address table plus one, or `0xFE` for "none".
    pub connection: u8,
    /// Group object number.
    pub object: u8,
}

/// The tables of one device and where they go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tables {
    /// The group objects by number; gaps in the numbering are absent.
    pub objects: Vec<GroupObject>,
    /// Address of the address table.
    pub address_table: u32,
    /// The group addresses, ascending.
    pub addresses: Vec<GroupAddress>,
    /// Address of the association table.
    pub association_table: u32,
    /// The association table entries, in table order.
    pub associations: Vec<Association>,
    /// Address of the group object table.
    pub object_table: u32,
    /// New segment layout, when the product places its tables dynamically.
    pub placement: Option<TablePlacement>,
}

/// The size code of a group object value of `bits` bits. Codes up to 14 bytes are verified
/// for the sizes that occur in the recorded downloads (1, 2 and 4 bits, 1 and 2 bytes); the
/// others follow the published table.
pub fn size_code(bits: u16) -> Option<u8> {
    Some(match bits {
        1..=7 => (bits - 1) as u8,
        8 => 7,
        16 => 8,
        24 => 9,
        32 => 10,
        48 => 11,
        64 => 12,
        80 => 13,
        112 => 14,
        _ => return None,
    })
}

/// Collect the group objects of `program` as `device` uses them.
pub fn group_objects(
    program: &ApplicationProgram,
    configuration: &Configuration,
    device: &DeviceData,
    addresses: &GroupAddresses,
) -> Result<Vec<GroupObject>, Error> {
    let mut objects: Vec<GroupObject> = program
        .com_objects
        .iter()
        .map(|object| GroupObject {
            number: object.number,
            active: false,
            attributes: object.attributes,
            links: Vec::new(),
        })
        .collect();
    objects.sort_by_key(|object| object.number);
    if objects.windows(2).any(|pair| match pair {
        [first, second] => first.number == second.number,
        _ => false,
    }) {
        return Err(Error::Unsupported(
            "two group objects with the same number".into(),
        ));
    }
    let find = |objects: &[GroupObject], reference: usize| -> Result<usize, Error> {
        let reference = program.com_object_refs.get(reference).ok_or_else(|| {
            Error::UnknownReference(format!("group object reference #{reference}"))
        })?;
        let object = program
            .com_object(&reference.object)
            .ok_or_else(|| Error::UnknownReference(reference.object.clone()))?;
        objects
            .binary_search_by_key(&object.number, |candidate| candidate.number)
            .map_err(|_| Error::UnknownReference(reference.object.clone()))
    };
    // First reference in use decides the attributes ...
    for (index, reference) in program.com_object_refs.iter().enumerate() {
        if !configuration.object_active(index) {
            continue;
        }
        let at = find(&objects, index)?;
        let base = program
            .com_object(&reference.object)
            .map(|object| object.attributes)
            .unwrap_or_default();
        if let Some(object) = objects.get_mut(at)
            && !object.active
        {
            object.active = true;
            object.attributes = reference.overrides.or(base);
        }
    }
    // ... unless the project links another reference in use of the same object.
    for entry in &device.object_links {
        let index = program
            .com_object_ref_index(&entry.reference)
            .ok_or_else(|| Error::UnknownReference(entry.reference.clone()))?;
        if !configuration.object_active(index) {
            continue;
        }
        let at = find(&objects, index)?;
        let (Some(object), Some(reference)) =
            (objects.get_mut(at), program.com_object_refs.get(index))
        else {
            continue;
        };
        let base = program
            .com_object(&reference.object)
            .map(|object| object.attributes)
            .unwrap_or_default();
        object.attributes = entry.overrides.or(reference.overrides).or(base);
        for id in &entry.links {
            let address = addresses
                .get(id)
                .ok_or_else(|| Error::UnknownReference(id.clone()))?;
            object.links.push(address);
        }
    }
    Ok(objects)
}

/// The configuration byte flags of an object: priority in bits 0–1 (low 3, high 1, alert 2),
/// communication 0x04 (only for linked objects), read 0x08, write 0x10, transmit 0x40,
/// update 0x80. Bit 0x20 belongs to the application and is never set here.
fn config_flags(object: &GroupObject) -> u8 {
    let attributes = &object.attributes;
    let priority = match attributes.priority.unwrap_or(ObjectPriority::Low) {
        ObjectPriority::Low => 3,
        ObjectPriority::High => 1,
        ObjectPriority::Alert => 2,
    };
    let flag = |value: Option<bool>, bit: u8| if value == Some(true) { bit } else { 0 };
    let linked = object.active && !object.links.is_empty();
    priority
        | flag(attributes.communication.filter(|_| linked), 0x04)
        | flag(attributes.read, 0x08)
        | flag(attributes.write, 0x10)
        | flag(attributes.transmit, 0x40)
        | flag(attributes.update, 0x80)
}

fn overflow(message: String) -> Error {
    Error::TableOverflow(message)
}

fn narrow(value: usize, what: &str) -> Result<u8, Error> {
    u8::try_from(value).map_err(|_| overflow(format!("{what} {value} does not fit one byte")))
}

/// Entries per object-number slot, then the remaining associations (BCU 2).
///
/// Entry `n` of the first part belongs to object `n`: its sending address, or `0xFE` when it
/// has none (verified against a recorded download). Further addresses of an object are
/// appended, and then moved into unused slots where the behaviour notes allow it (unverified).
fn slot_associations(
    objects: &[GroupObject],
    addresses: &[GroupAddress],
) -> Result<Vec<Association>, Error> {
    const NONE: u8 = 0xFE;
    let slots = objects
        .iter()
        .map(|object| usize::from(object.number) + 1)
        .max()
        .unwrap_or(0);
    let mut table = Vec::with_capacity(slots);
    for number in 0..slots {
        table.push(Association {
            connection: NONE,
            object: narrow(number, "group object number")?,
        });
    }
    let mut tail = Vec::new();
    for object in objects.iter().filter(|object| object.active) {
        for (position, address) in object.links.iter().enumerate() {
            let index = addresses.binary_search(address).unwrap_or(usize::MAX - 1);
            let entry = Association {
                connection: narrow(index + 1, "connection number")?,
                object: narrow(usize::from(object.number), "group object number")?,
            };
            match table.get_mut(usize::from(object.number)) {
                Some(slot) if position == 0 => *slot = entry,
                _ => tail.push(entry),
            }
        }
    }
    // Move appended entries into slots that are still unused, unless the entry's address is
    // already the sending address of some object.
    for slot in 0..table.len() {
        if table.get(slot).is_none_or(|entry| entry.connection != NONE) {
            continue;
        }
        let candidate = tail.iter().position(|entry| {
            !table.iter().enumerate().any(|(at, other)| {
                usize::from(other.object) == at && other.connection == entry.connection
            })
        });
        if let Some(candidate) = candidate
            && let Some(target) = table.get_mut(slot)
        {
            *target = tail.remove(candidate);
        }
    }
    table.extend(tail);
    Ok(table)
}

/// Real associations only (BIM M112): sending addresses first, then by object number, then by
/// connection number. Verified for one address per object.
fn sorted_associations(
    objects: &[GroupObject],
    addresses: &[GroupAddress],
) -> Result<Vec<Association>, Error> {
    let mut keyed = Vec::new();
    for object in objects.iter().filter(|object| object.active) {
        for (position, address) in object.links.iter().enumerate() {
            let index = addresses.binary_search(address).unwrap_or(usize::MAX - 1);
            let entry = Association {
                connection: narrow(index + 1, "connection number")?,
                object: narrow(usize::from(object.number), "group object number")?,
            };
            keyed.push((position != 0, entry.object, entry.connection, entry));
        }
    }
    keyed.sort_by_key(|(non_sending, object, connection, _)| (*non_sending, *object, *connection));
    Ok(keyed.into_iter().map(|(_, _, _, entry)| entry).collect())
}

/// Stop sending the bytes of a table reservation: `header + 2 × max entries` bytes, or up to
/// the end of the segment when the product states no capacity.
fn clear_reservation(
    image: &mut Image,
    program: &ApplicationProgram,
    table: &TableLocation,
    header: u32,
) -> Result<u32, Error> {
    let start = program.table_address(table)?;
    let segment = program
        .segment(&table.segment)
        .ok_or_else(|| Error::UnknownReference(table.segment.clone()))?;
    let segment_end = segment.address.saturating_add(segment.size);
    let size = match table.max_entries {
        Some(entries) => header.saturating_add(entries.saturating_mul(2)),
        None => segment_end.saturating_sub(start),
    };
    for address in start..start.saturating_add(size).min(segment_end) {
        image.set_send(address, false);
    }
    Ok(size)
}

/// Build the tables and write them into `image`.
///
/// `steps` is the load procedure after selection; it is read for the flags of the address
/// table segment, which decide about the extra byte on BCU 2.
///
/// Returns `None` when the product declares no address table.
pub fn apply(
    image: &mut Image,
    program: &ApplicationProgram,
    configuration: &Configuration,
    device: &DeviceData,
    group_addresses: &GroupAddresses,
    profile: &MaskProfile,
    steps: &[LoadStep],
) -> Result<Option<Tables>, Error> {
    let (Some(address_location), Some(association_location), Some(object_location)) = (
        &program.address_table,
        &program.association_table,
        &program.com_object_table,
    ) else {
        return Ok(None);
    };
    let objects = group_objects(program, configuration, device, group_addresses)?;
    let mut addresses: Vec<GroupAddress> = objects
        .iter()
        .filter(|object| object.active)
        .flat_map(|object| object.links.iter().copied())
        .collect();
    addresses.sort();
    addresses.dedup();
    if addresses.len() > 254 {
        return Err(overflow(format!("{} group addresses", addresses.len())));
    }
    let associations = match profile.tables {
        TableSystem::System2 => slot_associations(&objects, &addresses)?,
        TableSystem::System7 => sorted_associations(&objects, &addresses)?,
    };
    if associations.len() > 255 {
        return Err(overflow(format!("{} associations", associations.len())));
    }

    let address_table = program.table_address(address_location)?;
    if image.byte(address_table).is_none() {
        return Err(Error::OutOfRange(format!(
            "address table at {address_table:04X} is not in the image"
        )));
    }
    let address_bytes = 3 + 2 * addresses.len() as u32;
    let association_bytes = 1 + 2 * associations.len() as u32;
    let address_room = clear_reservation(image, program, address_location, 3)?;
    let association_room = clear_reservation(image, program, association_location, 1)?;

    let (association_table, placement) = if program.dynamic_table_management {
        // The checksummed address table segment of a BCU 2 gets one byte more than the table.
        let checksummed = steps.iter().any(|step| {
            matches!(
                step.action,
                Action::AbsSegment { target: Target::Index(1), flags, .. } if flags & 0x80 != 0
            )
        });
        let extra = u32::from(profile.extra_table_byte && checksummed);
        let used = address_bytes + extra;
        if used + association_bytes > address_room.saturating_add(association_room) {
            return Err(overflow(format!(
                "address and association tables need {} bytes, {} are reserved",
                used + association_bytes,
                address_room.saturating_add(association_room)
            )));
        }
        let start = address_table + used;
        let placement = TablePlacement {
            address_table_size: u16::try_from(used)
                .map_err(|_| overflow("address table size".into()))?,
            association_table: u16::try_from(start)
                .map_err(|_| overflow("association table address".into()))?,
        };
        (start, Some(placement))
    } else {
        if address_bytes > address_room {
            return Err(overflow(format!("{} group addresses", addresses.len())));
        }
        if association_bytes > association_room {
            return Err(overflow(format!("{} associations", associations.len())));
        }
        (program.table_address(association_location)?, None)
    };

    // Address table. The individual address slot is skipped.
    image.write(address_table, narrow(addresses.len() + 1, "address count")?)?;
    for (index, address) in addresses.iter().enumerate() {
        let at = address_table + 3 + 2 * index as u32;
        let [high, low] = address.raw().to_be_bytes();
        image.write(at, high)?;
        image.write(at + 1, low)?;
    }

    // Association table.
    image.write(
        association_table,
        narrow(associations.len(), "association count")?,
    )?;
    for (index, entry) in associations.iter().enumerate() {
        let at = association_table + 1 + 2 * index as u32;
        image.write(at, entry.connection)?;
        image.write(at + 1, entry.object)?;
    }
    if profile.tables == TableSystem::System2
        && (0x0100..=0x01FF).contains(&association_table)
        && image.byte(SYSTEM2_ASSOCIATION_POINTER).is_some()
    {
        // Observed on one recorded download.
        image.write(
            SYSTEM2_ASSOCIATION_POINTER,
            (association_table & 0xFF) as u8,
        )?;
    }

    let object_table = program.table_address(object_location)?;
    write_object_table(image, &objects, object_table, profile.tables)?;

    Ok(Some(Tables {
        objects,
        address_table,
        addresses,
        association_table,
        associations,
        object_table,
        placement,
    }))
}

/// Rewrite the configuration and type bytes of the group object table in place.
///
/// The count, the pointer to the RAM flags and each object's value pointer keep the product's
/// content and are sent. For an object in use the configuration byte keeps the product's bit
/// 0x20 and takes everything else from the object's attributes, and the type byte keeps the
/// product's top two bits and takes the size code. For an object not in use the type byte is
/// left alone; its configuration byte is rebuilt the same way on BCU 2 (from the object's own
/// attributes, without the communication bit) and only loses the communication bit on
/// BIM M112. The recorded downloads tell the two apart: swapping the rules between the
/// families breaks the match on both.
fn write_object_table(
    image: &mut Image,
    objects: &[GroupObject],
    table: u32,
    system: TableSystem,
) -> Result<(), Error> {
    let (header, entry, pointer) = match system {
        TableSystem::System2 => (2, 3, 1),
        TableSystem::System7 => (3, 4, 2),
    };
    let read = |image: &Image, address: u32| {
        image
            .byte(address)
            .ok_or_else(|| Error::OutOfRange(format!("group object table byte {address:04X}")))
    };
    // The product's own count wins over the number of objects the product data lists.
    let count = u32::from(read(image, table)?);
    for offset in 0..header {
        let value = read(image, table + offset)?;
        image.write(table + offset, value)?;
    }
    for number in 0..count {
        let base = table + header + entry * number;
        let config_at = base + pointer;
        let type_at = config_at + 1;
        for address in base..=type_at {
            let value = read(image, address)?;
            image.write(address, value)?;
        }
        let Ok(at) = objects.binary_search_by_key(&number, |object| u32::from(object.number))
        else {
            continue;
        };
        let Some(object) = objects.get(at) else {
            continue;
        };
        let old_config = read(image, config_at)?;
        let flags = config_flags(object);
        let config = if object.active || system == TableSystem::System2 {
            (old_config & 0x20) | (flags & !0x20)
        } else {
            old_config & !0x04
        };
        image.write(config_at, config)?;
        if object.active {
            let bits = object.attributes.size_in_bit.unwrap_or(1);
            let code = size_code(bits).ok_or_else(|| {
                Error::Unsupported(format!(
                    "group object {} with a {bits}-bit value",
                    object.number
                ))
            })?;
            let old_type = read(image, type_at)?;
            image.write(type_at, (old_type & 0xC0) | code)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(number: u16, links: &[u16]) -> GroupObject {
        GroupObject {
            number,
            active: true,
            attributes: ObjectAttributes {
                communication: Some(true),
                write: Some(true),
                ..ObjectAttributes::default()
            },
            links: links
                .iter()
                .map(|raw| GroupAddress::from_raw(*raw))
                .collect(),
        }
    }

    fn pairs(table: &[Association]) -> Vec<(u8, u8)> {
        table.iter().map(|e| (e.connection, e.object)).collect()
    }

    #[test]
    fn size_codes() {
        assert_eq!(size_code(1), Some(0));
        assert_eq!(size_code(4), Some(3));
        assert_eq!(size_code(7), Some(6));
        assert_eq!(size_code(8), Some(7));
        assert_eq!(size_code(16), Some(8));
        assert_eq!(size_code(112), Some(14));
        assert_eq!(size_code(40), None);
        assert_eq!(size_code(0), None);
    }

    #[test]
    fn flags() {
        let mut linked = object(0, &[1]);
        linked.attributes.transmit = Some(true);
        linked.attributes.priority = Some(ObjectPriority::High);
        assert_eq!(config_flags(&linked), 0x01 | 0x04 | 0x10 | 0x40);
        let unlinked = object(1, &[]);
        assert_eq!(config_flags(&unlinked), 0x03 | 0x10);
        let mut inactive = object(2, &[1]);
        inactive.active = false;
        assert_eq!(config_flags(&inactive), 0x03 | 0x10);
    }

    #[test]
    fn slot_order_single_links() {
        let objects = [object(0, &[30]), object(1, &[]), object(3, &[10])];
        let addresses = [GroupAddress::from_raw(10), GroupAddress::from_raw(30)];
        let table = slot_associations(&objects, &addresses).unwrap();
        assert_eq!(pairs(&table), [(2, 0), (0xFE, 1), (0xFE, 2), (1, 3)]);
    }

    #[test]
    fn slot_order_extra_links_fill_gaps() {
        // Object 0 sends on 10 and also listens on 20 and 30; object 2 sends on 30.
        let objects = [object(0, &[10, 20, 30]), object(1, &[]), object(2, &[30])];
        let addresses = [10, 20, 30].map(GroupAddress::from_raw);
        let table = slot_associations(&objects, &addresses).unwrap();
        // (2, 0) may move into the unused slot 1; (3, 0) may not, because connection 3 is the
        // sending address of object 2, so it stays behind the slots.
        assert_eq!(pairs(&table), [(1, 0), (2, 0), (3, 2), (3, 0)]);
    }

    #[test]
    fn sorted_order() {
        let objects = [object(5, &[30, 10]), object(2, &[20])];
        let addresses = [10, 20, 30].map(GroupAddress::from_raw);
        let table = sorted_associations(&objects, &addresses).unwrap();
        assert_eq!(pairs(&table), [(2, 2), (3, 5), (1, 5)]);
    }
}
