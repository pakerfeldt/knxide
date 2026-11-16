//! Everything a download of one device consists of, before any request is sent.

use crate::config::Configuration;
use crate::device::{DeviceData, DownloadStamp, GroupAddresses};
use crate::error::Error;
use crate::image::{Image, encode};
use crate::mask::{MaskProfile, Resource, TableSystem};
use crate::procedure::{
    self, Action, ControlVariable, DownloadKind, LoadStep, normalise, patch_table_placement,
    remove_foreign_steps, transform_for_partial,
};
use crate::product::{
    ApplicationProgram, InvisibleParameters, LoadProcedureStyle, ParameterKind, ParameterLocation,
};
use crate::tables::{self, Tables};

/// The identity an application reports once loaded: manufacturer, application number and
/// version. It travels in task segment events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationId {
    /// Manufacturer identifier.
    pub manufacturer: u16,
    /// Application number.
    pub number: u16,
    /// Application version.
    pub version: u8,
}

impl std::fmt::Display for ApplicationId {
    /// `000C/3245 version 11`: manufacturer, application number and version, in hex.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:04X}/{:04X} version {:02X}",
            self.manufacturer, self.number, self.version
        )
    }
}

impl ApplicationId {
    /// The five bytes as they appear on the bus and in device memory.
    pub fn to_bytes(self) -> [u8; 5] {
        let [m1, m2] = self.manufacturer.to_be_bytes();
        let [n1, n2] = self.number.to_be_bytes();
        [m1, m2, n1, n2, self.version]
    }
}

/// What the image holds where a mask's hardware identification is compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareExpectation {
    /// The bytes the device should report.
    pub data: Vec<u8>,
    /// Per byte, the bits that count.
    pub mask: Vec<u8>,
}

impl HardwareExpectation {
    /// Whether what the device reports fits: the bytes both sides have are equal under the
    /// mask.
    pub fn accepts(&self, device: &[u8]) -> bool {
        device
            .iter()
            .zip(&self.data)
            .zip(&self.mask)
            .all(|((device, image), mask)| device & mask == image & mask)
    }
}

/// A prepared download: memory image, tables and the procedure to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    /// Rules of the device's mask version.
    pub profile: MaskProfile,
    /// What the download covers.
    pub kind: DownloadKind,
    /// Parameter values and visibility.
    pub configuration: Configuration,
    /// The memory image.
    pub image: Image,
    /// The communication tables; `None` when the product has none or they are not part of
    /// the download.
    pub tables: Option<Tables>,
    /// The load procedure, rewritten and ready to run.
    pub procedure: Vec<LoadStep>,
    /// Identity of the application.
    pub application: ApplicationId,
    /// Interface type the application expects.
    pub pei_type: u8,
    /// Checksums the project recorded after the last download, compared with the device before
    /// a partial download.
    pub stamp: Option<DownloadStamp>,
    /// Identifier of the application program, for example `M-0001_A-0002-03-0000`.
    pub application_name: String,
    /// What the device's hardware identification is compared with before anything is written;
    /// `None` when the image holds nothing at the place the mask names.
    pub hardware: Option<HardwareExpectation>,
    /// The product asks not to compare the device's manufacturer.
    pub skip_manufacturer_check: bool,
    /// The product asks not to look at the device's second application program before a
    /// partial download.
    pub ignore_second_application: bool,
    /// The full download to turn to when the device turns out not to hold this application:
    /// set by [`Download::prepare`] for a partial download nobody asked for explicitly.
    pub fallback: Option<Box<Download>>,
}

/// First address of the BCU 2 memory header fields this crate fills in.
const SYSTEM2_APPLICATION_ID: u32 = 0x0103;
/// Address of the BCU 2 header byte that holds the expected interface type.
const SYSTEM2_PEI_TYPE: u32 = 0x0109;

impl Download {
    /// Prepare what "download the application" does: a full download, or, when the product
    /// prefers it (`PreferPartialDownloadIfApplicationLoaded`), a partial one with the full
    /// one as [`Download::fallback`]. Which of the two runs is decided while planning, by
    /// asking the device whether it holds this application.
    ///
    /// The rule is taken from behaviour notes and reproduces all three recorded downloads
    /// (two full ones, one partial). What a project file says about the device being loaded
    /// plays no part.
    pub fn prepare(
        program: &ApplicationProgram,
        device: &DeviceData,
        group_addresses: &GroupAddresses,
    ) -> Result<Self, Error> {
        let full = Self::build(program, device, group_addresses, DownloadKind::FULL)?;
        if !program.options.prefer_partial_download {
            return Ok(full);
        }
        let mut partial = Self::build(program, device, group_addresses, DownloadKind::PARTIAL)?;
        partial.fallback = Some(Box::new(full));
        Ok(partial)
    }

    /// This download or its fallback, whichever is of `kind`.
    pub fn variant(&self, kind: DownloadKind) -> &Self {
        match &self.fallback {
            Some(fallback) if self.kind != kind && fallback.kind == kind => fallback,
            _ => self,
        }
    }

    /// Prepare a download of exactly `kind` for `device`, an instance of `program`. A partial
    /// download built this way stops when the device does not hold the application.
    pub fn build(
        program: &ApplicationProgram,
        device: &DeviceData,
        group_addresses: &GroupAddresses,
        kind: DownloadKind,
    ) -> Result<Self, Error> {
        let profile = MaskProfile::for_mask(program.mask_version)?;
        if program.load_procedure_style != LoadProcedureStyle::Product {
            return Err(Error::Unsupported(
                "load procedures taken from the mask (default or merged style)".into(),
            ));
        }
        let options = &program.options;
        if options.little_endian_parameters {
            return Err(Error::Unsupported("little-endian parameters".into()));
        }
        if options.invisible_parameters == InvisibleParameters::DefaultValue {
            return Err(Error::Unsupported(
                "default values for invisible parameters".into(),
            ));
        }
        if options.ignore_property_write_errors {
            return Err(Error::Unsupported("ignoring property write errors".into()));
        }
        if options.patch_manufacturer_id {
            return Err(Error::Unsupported(
                "patching the manufacturer into the task segment".into(),
            ));
        }

        let configuration = Configuration::evaluate(program, device)?;
        let mut image = Image::from_program(program, kind, &profile)?;
        if kind.parameters {
            image.apply_parameters(program, &configuration)?;
        }

        // Legacy product switches act like steps at the very start of the procedure.
        let mut steps = Vec::new();
        if options.no_optimistic_write {
            steps.push(LoadStep::new(Action::SetControlVariable {
                variable: ControlVariable::OptimisticWrite,
                value: false,
            }));
        }
        if options.no_memory_verify_mode {
            steps.push(LoadStep::new(Action::SetControlVariable {
                variable: ControlVariable::MemoryAutoVerify,
                value: false,
            }));
        }
        let mut product_steps = program.load_procedure.clone();
        remove_foreign_steps(&mut product_steps, &profile, &program.id);
        steps.extend(procedure::select(&product_steps, kind));
        normalise(&mut steps);

        let tables = if kind.group_communication {
            tables::apply(
                &mut image,
                program,
                &configuration,
                device,
                group_addresses,
                &profile,
                &steps,
            )?
        } else {
            None
        };
        if let Some(placement) = tables.as_ref().and_then(|tables| tables.placement) {
            patch_table_placement(&mut steps, placement);
        }

        let application = ApplicationId {
            manufacturer: program.manufacturer,
            number: program.application_number,
            version: program.application_version,
        };
        if profile.tables == TableSystem::System2 {
            // Observed on one recorded download: the application identity and the interface
            // type are filled into the memory header where the product image has room for them.
            if (0..5).all(|offset| image.byte(SYSTEM2_APPLICATION_ID + offset).is_some()) {
                for (offset, byte) in (0u32..).zip(application.to_bytes()) {
                    image.write(SYSTEM2_APPLICATION_ID + offset, byte)?;
                }
            }
            if image.byte(SYSTEM2_PEI_TYPE).is_some() {
                image.write(SYSTEM2_PEI_TYPE, program.pei_type)?;
            }
        }

        if !kind.full {
            transform_for_partial(&mut steps, kind, &profile);
            normalise(&mut steps);
        }

        let hardware = hardware_expectation(program, &configuration, &image, &profile)?;
        Ok(Self {
            profile,
            kind,
            configuration,
            image,
            tables,
            procedure: steps,
            application,
            pei_type: program.pei_type,
            stamp: device.stamp.clone().filter(|_| device.has_loaded_image),
            application_name: program.id.clone(),
            hardware,
            skip_manufacturer_check: options.skip_manufacturer_check,
            ignore_second_application: options.ignore_second_application,
            fallback: None,
        })
    }
}

/// What the image holds at the place the mask's hardware identification is compared with.
///
/// For a memory location: the image bytes there, under the product segment's mask. For a
/// property: the values of the product's parameters located in that property (a parameter in
/// use contributes its value, one that is not leaves zeros; either way its bytes count).
///
/// From behaviour notes. Two recorded downloads pass the comparison built here (one byte of
/// memory on mask `0021`, a six-byte property on mask `0705`); a mismatch has not been seen.
fn hardware_expectation(
    program: &ApplicationProgram,
    configuration: &Configuration,
    image: &Image,
    profile: &MaskProfile,
) -> Result<Option<HardwareExpectation>, Error> {
    let Some(check) = profile.hardware else {
        return Ok(None);
    };
    let length = usize::from(check.length);
    match check.image {
        Resource::Memory(address) => {
            let address = u32::from(address);
            let Some(segment) = program.segments.iter().find(|segment| {
                segment.address <= address && address - segment.address < segment.size
            }) else {
                return Ok(None);
            };
            let offset = (address - segment.address) as usize;
            let mut data = Vec::new();
            let mut mask = Vec::new();
            for at in 0..length {
                let Some(byte) = image.byte(address + at as u32) else {
                    break;
                };
                if offset + at >= segment.size as usize {
                    break;
                }
                data.push(byte);
                mask.push(match &segment.mask {
                    Some(mask) => mask.get(offset + at).copied().unwrap_or(0),
                    None => 0xFF,
                });
            }
            Ok((!data.is_empty()).then_some(HardwareExpectation { data, mask }))
        }
        Resource::Property { object, property } => {
            let mut data = vec![0u8; length];
            let mut mask = vec![0u8; length];
            let mut found = false;
            for (index, reference) in program.parameter_refs.iter().enumerate() {
                let Some(parameter) = program.parameter(&reference.parameter) else {
                    continue;
                };
                let ParameterLocation::Property {
                    object: Some(at_object),
                    property: at_property,
                    bit,
                } = parameter.location
                else {
                    continue;
                };
                if at_object != object || at_property != u16::from(property) {
                    continue;
                }
                let Some(kind) = program.parameter_type(&parameter.parameter_type) else {
                    continue;
                };
                if kind.size_in_bit == 0 {
                    continue;
                }
                found = true;
                let first = bit as usize;
                let end = first + kind.size_in_bit as usize;
                for slot in mask.iter_mut().take(end.div_ceil(8)).skip(first / 8) {
                    *slot = 0xFF;
                }
                if !configuration.parameter_active(index) {
                    continue;
                }
                let value = configuration.value(program, index)?;
                let bits = match &kind.kind {
                    ParameterKind::Binary(members) => {
                        let bytes = members
                            .iter()
                            .find(|(member, _)| member.trim() == value.trim())
                            .map(|(_, bytes)| bytes.as_slice())
                            .ok_or_else(|| Error::Invalid {
                                what: "byte string parameter value",
                                value: value.chars().take(40).collect(),
                            })?;
                        bytes
                            .iter()
                            .flat_map(|byte| (0..8).map(move |at| byte >> (7 - at) & 1 == 1))
                            .take(kind.size_in_bit as usize)
                            .collect()
                    }
                    _ => encode(kind, value, program.options.zero_terminate_text)?,
                };
                for (position, bit) in (first..end).zip(bits) {
                    if let Some(byte) = data.get_mut(position / 8) {
                        let flag = 0x80u8 >> (position % 8);
                        *byte = if bit { *byte | flag } else { *byte & !flag };
                    }
                }
            }
            Ok(found.then_some(HardwareExpectation { data, mask }))
        }
    }
}
