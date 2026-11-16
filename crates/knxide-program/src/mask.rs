//! What this crate knows about each supported mask version (device system type).
//!
//! Only masks with a recorded download to check against are listed: `0021` (BCU 2) and `0701`,
//! `0705` (BIM M112). Everything in a profile is verified against a recorded download unless
//! its documentation says otherwise.

use crate::error::Error;

/// Layout family of the three communication tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSystem {
    /// BCU 2: one-byte pointers, three bytes per group object, one association slot per object.
    System2,
    /// BIM M112: two-byte pointers, four bytes per group object, real associations only.
    System7,
}

/// How load state machine events reach the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadControl {
    /// Property write to the interface object with the state machine's index, property 5
    /// (load state control), ten bytes. The response carries the new state.
    Property,
    /// Memory write of an eleven-byte record, then a read of one status byte per state machine.
    Memory {
        /// Address of the control record.
        control: u16,
        /// Address of the status byte of state machine 1; state machine `n` is at
        /// `status + n - 1`. Verified for state machines 1 to 3.
        status: u16,
    },
}

/// Where a mask keeps one piece of device state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// Element 1 of an interface object property.
    Property {
        /// Object index.
        object: u8,
        /// Property identifier.
        property: u8,
    },
    /// Standard memory, from this address.
    Memory(u16),
}

/// The hardware identification a tool compares with the image before it downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareCheck {
    /// Where the device keeps it.
    pub read: Resource,
    /// Its length in bytes.
    pub length: u8,
    /// Where the image holds the value to compare with: a memory address, or a property that
    /// a parameter of the product is located in.
    pub image: Resource,
}

/// Rules that depend on the mask version.
///
/// The resource locations (`serial_number` to `hardware`) come from behaviour notes on the
/// mask data a tool ships with. A recorded download confirms each read that occurs in it:
/// the serial number on all three masks, the manufacturer and hardware reads on `0021` and
/// `0705`, the load states and the application identity on `0705`. The rest is unverified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaskProfile {
    /// Mask version, as answered by a device descriptor read (type 0).
    pub mask_version: u16,
    /// Communication table layout.
    pub tables: TableSystem,
    /// Transport of load state machine events.
    pub load_control: LoadControl,
    /// Whether the device can echo memory writes when bit 2 of the device object's device
    /// control property (object 0, property 14) is set.
    pub verify_mode: bool,
    /// Whether an address table segment flagged as checksummed (segment flags bit 7) gets one
    /// byte more than the table needs when tables are placed dynamically.
    pub extra_table_byte: bool,
    /// Address of the two individual address bytes inside the address table. They belong to
    /// individual address programming and are never part of an application download.
    pub individual_address: u16,
    /// Whether the group object table has a load state machine of its own (object index 5).
    /// Only System B masks (`07B0` and its variants) do; none of the masks here.
    pub object_table_load_control: bool,
    /// The device's serial number, six bytes.
    pub serial_number: Option<Resource>,
    /// Address of a byte that tells how the device is managed: 0 for load state machines,
    /// anything else for the older direct style. `None` when the mask fixes the style.
    pub management_style: Option<u16>,
    /// Load state of the application program, one byte.
    pub application_load_status: Option<Resource>,
    /// Whether [`Self::application_load_status`] exists only for devices managed through load
    /// state machines (see [`Self::management_style`]).
    pub load_status_needs_state_machines: bool,
    /// Identity of the loaded application program: manufacturer, application number, version.
    pub application_id: Option<Resource>,
    /// Load state of the second application program (the interface program), one byte.
    pub second_application_load_status: Option<Resource>,
    /// Identity of the second application program.
    pub second_application_id: Option<Resource>,
    /// The device's manufacturer identifier, two bytes.
    pub manufacturer_id: Option<Resource>,
    /// The hardware identification to compare before a download.
    pub hardware: Option<HardwareCheck>,
}

/// Number of system interface objects with a known type; see [`MaskProfile::object_type`].
const SYSTEM_OBJECTS: u8 = 5;

/// Device object, PID_SERIAL_NUMBER.
const DEVICE_SERIAL_NUMBER: Resource = Resource::Property {
    object: 0,
    property: 11,
};
/// Device object, PID_MANUFACTURER_ID.
const DEVICE_MANUFACTURER_ID: Resource = Resource::Property {
    object: 0,
    property: 12,
};
/// Device object, PID_HARDWARE_TYPE.
const HARDWARE_TYPE: Resource = Resource::Property {
    object: 0,
    property: 78,
};
/// Application program object, PID_PROGRAM_VERSION.
const APPLICATION_PROGRAM_VERSION: Resource = Resource::Property {
    object: 3,
    property: 13,
};
/// Interface program object, PID_PROGRAM_VERSION.
const INTERFACE_PROGRAM_VERSION: Resource = Resource::Property {
    object: 4,
    property: 13,
};

impl MaskProfile {
    /// The profile for `mask_version`.
    pub fn for_mask(mask_version: u16) -> Result<Self, Error> {
        let profile = match mask_version {
            0x0021 => Self {
                mask_version,
                tables: TableSystem::System2,
                load_control: LoadControl::Property,
                verify_mode: true,
                extra_table_byte: true,
                individual_address: 0x0117,
                object_table_load_control: false,
                serial_number: Some(DEVICE_SERIAL_NUMBER),
                management_style: Some(0x0115),
                application_load_status: Some(Resource::Property {
                    object: 3,
                    property: 5,
                }),
                load_status_needs_state_machines: true,
                application_id: Some(APPLICATION_PROGRAM_VERSION),
                second_application_load_status: None,
                second_application_id: None,
                manufacturer_id: Some(DEVICE_MANUFACTURER_ID),
                hardware: Some(HardwareCheck {
                    read: Resource::Property {
                        object: 0,
                        property: 17,
                    },
                    length: 1,
                    image: Resource::Memory(0x010C),
                }),
            },
            0x0701 => Self {
                mask_version,
                tables: TableSystem::System7,
                load_control: LoadControl::Memory {
                    control: 0x0104,
                    status: 0xB6EA,
                },
                verify_mode: false,
                extra_table_byte: false,
                individual_address: 0x4001,
                object_table_load_control: false,
                serial_number: Some(DEVICE_SERIAL_NUMBER),
                management_style: None,
                application_load_status: Some(Resource::Memory(0xB6EC)),
                load_status_needs_state_machines: false,
                application_id: Some(APPLICATION_PROGRAM_VERSION),
                second_application_load_status: Some(Resource::Memory(0xB6ED)),
                second_application_id: Some(INTERFACE_PROGRAM_VERSION),
                manufacturer_id: None,
                hardware: None,
            },
            0x0705 => Self {
                mask_version,
                tables: TableSystem::System7,
                load_control: LoadControl::Property,
                verify_mode: true,
                extra_table_byte: false,
                individual_address: 0x4001,
                object_table_load_control: false,
                serial_number: Some(DEVICE_SERIAL_NUMBER),
                management_style: None,
                application_load_status: Some(Resource::Memory(0xB6EC)),
                load_status_needs_state_machines: false,
                application_id: Some(APPLICATION_PROGRAM_VERSION),
                second_application_load_status: Some(Resource::Memory(0xB6ED)),
                second_application_id: Some(INTERFACE_PROGRAM_VERSION),
                manufacturer_id: Some(DEVICE_MANUFACTURER_ID),
                hardware: Some(HardwareCheck {
                    read: HARDWARE_TYPE,
                    length: 6,
                    image: HARDWARE_TYPE,
                }),
            },
            other => return Err(Error::UnsupportedMask(other)),
        };
        Ok(profile)
    }

    /// Whether the mask belongs to the BIM M112 family, whose two application programs share
    /// one set of load state machines (1 to 3 for the first, 4 for the second).
    pub fn is_bim_m112(&self) -> bool {
        self.tables == TableSystem::System7
    }

    /// Interface object type of the system object at `index`: 0 device, 1 address table,
    /// 2 association table, 3 application program, 4 interface (PEI) program. `None` for other
    /// indices. Spec: 3/5/1 (Resources), interface object types.
    ///
    /// The mapping itself is standard; that it is what decides how a step is treated in a
    /// partial download is taken from behaviour notes and reproduced on one recorded download.
    pub fn object_type(&self, index: u8) -> Option<u16> {
        (index < SYSTEM_OBJECTS).then_some(u16::from(index))
    }

    /// Size in bytes of one element of property `property` of the system object at `index`,
    /// for the properties a load procedure touches. Sizes follow the property data types in the
    /// KNX standard (3/7/3, Standard Identifier Tables). Recorded downloads confirm properties
    /// 8, 14, 27 and 78.
    ///
    /// The tool that made the recordings never asked the device for a property description, so
    /// it must hold such a table too. Which table exactly is unknown.
    pub fn property_element_size(&self, index: u8, property: u16) -> Option<usize> {
        let object_type = self.object_type(index)?;
        let size = match (object_type, property) {
            // PID_OBJECT_TYPE, PDT_UNSIGNED_INT.
            (_, 1) => 2,
            // PID_PROGRAM_VERSION, PDT_GENERIC_05: manufacturer, application number, version.
            (_, 13) => 5,
            // PID_MCB_TABLE, PDT_GENERIC_08: one memory control block per element.
            (_, 27) => 8,
            // PID_SERVICE_CONTROL, PDT_UNSIGNED_INT.
            (0, 8) => 2,
            // PID_FIRMWARE_REVISION, PDT_UNSIGNED_CHAR.
            (0, 9) => 1,
            // PID_SERIAL_NUMBER, PDT_GENERIC_06.
            (0, 11) => 6,
            // PID_MANUFACTURER_ID, PDT_UNSIGNED_INT.
            (0, 12) => 2,
            // PID_DEVICE_CONTROL, PDT_BITSET8.
            (0, 14) => 1,
            // PID_ORDER_INFO, PDT_GENERIC_10.
            (0, 15) => 10,
            // PID_PEI_TYPE, PDT_UNSIGNED_CHAR.
            (0, 16) => 1,
            // PID_PORT_CONFIGURATION, PDT_UNSIGNED_CHAR.
            (0, 17) => 1,
            // PID_MAX_APDULENGTH, PDT_UNSIGNED_INT.
            (0, 56) => 2,
            // PID_HARDWARE_TYPE, PDT_GENERIC_06.
            (0, 78) => 6,
            _ => return None,
        };
        Some(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_unknown_masks() {
        assert_eq!(
            MaskProfile::for_mask(0x0021).unwrap().tables,
            TableSystem::System2
        );
        assert!(!MaskProfile::for_mask(0x0701).unwrap().verify_mode);
        assert_eq!(
            MaskProfile::for_mask(0x07B0),
            Err(Error::UnsupportedMask(0x07B0))
        );
    }

    #[test]
    fn object_types_and_sizes() {
        let profile = MaskProfile::for_mask(0x0705).unwrap();
        assert_eq!(profile.object_type(3), Some(3));
        assert_eq!(profile.object_type(5), None);
        assert_eq!(profile.property_element_size(0, 8), Some(2));
        assert_eq!(profile.property_element_size(3, 27), Some(8));
        assert_eq!(profile.property_element_size(3, 8), None);
        assert_eq!(profile.property_element_size(7, 27), None);
    }
}
