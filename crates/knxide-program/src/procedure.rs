//! Load procedures: the ordered steps a product prescribes for a download, and the fixed
//! rewriting they go through before execution.
//!
//! The rewriting has five parts, applied in this order by [`crate::Download::build`]:
//!
//! 1. [`remove_foreign_steps`]: drop load state machine steps the device's mask has no use
//!    for (reproduces two recorded downloads).
//! 2. [`select`]: keep the steps that belong to the kind of download.
//! 3. [`normalise`]: make connects and disconnects consistent.
//! 4. [`patch_table_placement`]: resize and move the table segments when the product places its
//!    tables dynamically (verified against two recorded downloads).
//! 5. [`transform_for_partial`] and [`normalise`] again: in a download that is not a full one,
//!    steps marked "auto" are kept, dropped or replaced depending on what they target
//!    (reproduced on one recorded download).

use std::fmt;

use crate::error::{Error, invalid};
use crate::mask::MaskProfile;
use crate::xml::Element;

/// What a download covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadKind {
    /// Full application download: the whole product image is sent and every step runs as
    /// written.
    pub full: bool,
    /// Parameters are part of the download.
    pub parameters: bool,
    /// The communication tables are part of the download.
    pub group_communication: bool,
}

impl DownloadKind {
    /// Everything, as for a device that has never been loaded.
    pub const FULL: Self = Self {
        full: true,
        parameters: true,
        group_communication: true,
    };
    /// Parameters and tables of a device that already runs this application: only bytes that
    /// parameters or tables own are sent, and the application is not unloaded.
    pub const PARTIAL: Self = Self {
        full: false,
        parameters: true,
        group_communication: true,
    };
}

/// The download kinds a step belongs to (attribute `AppliesTo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppliesTo {
    /// Every kind, and subject to [`transform_for_partial`]. The default.
    Auto,
    /// Every kind, as written.
    All,
    /// Full downloads.
    Full,
    /// Partial downloads that include parameters.
    Parameters,
    /// Partial downloads that include the tables.
    Group,
    /// Full downloads, or any download that includes parameters.
    FullOrParameters,
    /// Full downloads, or any download that includes the tables.
    FullOrGroup,
    /// Partial downloads that include both parameters and tables.
    ParametersAndGroup,
}

impl AppliesTo {
    fn parse(text: Option<&str>) -> Result<Self, Error> {
        Ok(match text {
            None | Some("auto") => Self::Auto,
            Some("all") => Self::All,
            Some("full") => Self::Full,
            Some("par") => Self::Parameters,
            Some("grp") => Self::Group,
            Some("full,par") => Self::FullOrParameters,
            Some("full,grp") => Self::FullOrGroup,
            Some("par,grp") => Self::ParametersAndGroup,
            Some(other) => return Err(invalid("AppliesTo", other)),
        })
    }

    /// Whether a step with this attribute is part of a download of `kind`. Unverified: the
    /// recorded downloads only contain steps that apply to every kind.
    pub fn includes(self, kind: DownloadKind) -> bool {
        let partial = !kind.full;
        match self {
            Self::Auto | Self::All => true,
            Self::Full => kind.full,
            Self::Parameters => partial && kind.parameters,
            Self::Group => partial && kind.group_communication,
            Self::FullOrParameters => kind.full || kind.parameters,
            Self::FullOrGroup => kind.full || kind.group_communication,
            Self::ParametersAndGroup => partial && kind.parameters && kind.group_communication,
        }
    }
}

/// The interface object or load state machine a step addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// By object index (attributes `LsmIdx`, `ObjIdx`).
    Index(u8),
    /// By object type and occurrence (attributes `ObjType`, `Occurrence`). Parsed, but the
    /// planner has no rules for it yet.
    ObjectType {
        /// Interface object type.
        object_type: u16,
        /// Zero-based occurrence among objects of that type.
        occurrence: u16,
    },
}

/// Switches a procedure can flip for the steps after it (step `LdCtrlSetControlVariable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlVariable {
    /// Whether an absolute segment step also writes the segment's content. Starts on.
    SegmentWrite,
    /// Whether verified memory writes first read and then write only what differs. Starts on.
    OptimisticWrite,
    /// Whether verified memory writes use the device's echo mode. Starts unset, which means
    /// "if the mask has it".
    MemoryAutoVerify,
    /// Stored but without known effect.
    VerifyOnWriteDirect,
}

/// Error causes a step can declare as harmless (child element `OnError` with `Ignore`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCause {
    /// The addressed resource does not exist or is protected.
    ResourceNotFound,
    /// A comparison or a verified write found other data than expected.
    CompareMismatch,
}

/// What a step does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Open the transport connection.
    Connect,
    /// Close the transport connection.
    Disconnect,
    /// Basic restart of the device; closes the connection.
    Restart,
    /// Wait. Nothing is sent.
    Delay {
        /// Duration in milliseconds.
        milliseconds: u16,
    },
    /// Load event "unload".
    Unload {
        /// The load state machine.
        target: Target,
    },
    /// Load event "start loading".
    Load {
        /// The load state machine.
        target: Target,
    },
    /// Load event "load completed".
    LoadCompleted {
        /// The load state machine.
        target: Target,
    },
    /// Allocate an absolute memory segment and, for EEPROM segments, write its content.
    AbsSegment {
        /// The load state machine.
        target: Target,
        /// Segment type.
        segment_type: u8,
        /// Start address.
        address: u16,
        /// Length in bytes.
        size: u16,
        /// Access rights byte.
        access: u8,
        /// Memory type; the low two bits equal to 3 mean EEPROM.
        memory_type: u8,
        /// Segment flags; bit 7 marks a checksummed segment.
        flags: u8,
    },
    /// Declare the segment that holds the task (application) header.
    TaskSegment {
        /// The load state machine.
        target: Target,
        /// Start address.
        address: u16,
    },
    /// Declare the task's entry points.
    TaskPtr {
        /// The load state machine.
        target: Target,
        /// Initialisation routine.
        init: u16,
        /// Save routine.
        save: u16,
        /// Serial routine.
        serial: u16,
    },
    /// First task control record.
    TaskCtrl1 {
        /// The load state machine.
        target: Target,
        /// Address field.
        address: u16,
        /// Count field.
        count: u8,
    },
    /// Second task control record.
    TaskCtrl2 {
        /// The load state machine.
        target: Target,
        /// Callback address.
        callback: u16,
        /// Address field.
        address: u16,
        /// First segment field.
        segment0: u16,
        /// Second segment field.
        segment1: u16,
    },
    /// Write a property value.
    WriteProperty {
        /// The interface object.
        object: Target,
        /// Property identifier.
        property: u16,
        /// First element, 1-based.
        start: u16,
        /// Number of elements.
        count: u16,
        /// Whether the answer must repeat the written data.
        verify: bool,
        /// Data given in the step itself.
        inline: Option<Vec<u8>>,
    },
    /// Read a property value and compare it with expected data.
    CompareProperty {
        /// The interface object.
        object: Target,
        /// Property identifier.
        property: u16,
        /// First element, 1-based.
        start: u16,
        /// Number of elements.
        count: u16,
        /// Expected data.
        inline: Option<Vec<u8>>,
        /// Bits to compare; all when absent.
        mask: Option<Vec<u8>>,
        /// Succeed on a mismatch instead of a match.
        invert: bool,
    },
    /// Read a property value from the device into the tool's picture of the device.
    LoadImageProperty {
        /// The interface object.
        object: Target,
        /// Property identifier.
        property: u16,
        /// First element, 1-based.
        start: u16,
        /// Number of elements.
        count: u16,
    },
    /// Write a range of standard memory from the image, or from data given in the step.
    WriteMemory {
        /// Start address.
        address: u16,
        /// Length in bytes.
        size: u16,
        /// Whether the write is verified.
        verify: bool,
        /// Data given in the step itself.
        inline: Option<Vec<u8>>,
    },
    /// Flip a switch for the following steps. Nothing is sent.
    SetControlVariable {
        /// The switch.
        variable: ControlVariable,
        /// Its new value.
        value: bool,
    },
    /// A step this crate has no rules for. Planning stops with an error when it is reached.
    Unsupported {
        /// The element name, for example `LdCtrlRelSegment`.
        element: String,
    },
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(index) => write!(f, "object {index}"),
            Self::ObjectType {
                object_type,
                occurrence,
            } => write!(f, "object type {object_type} #{occurrence}"),
        }
    }
}

impl fmt::Display for Action {
    /// A few words that tell a person which step this is.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect => f.write_str("connect"),
            Self::Disconnect => f.write_str("disconnect"),
            Self::Restart => f.write_str("restart"),
            Self::Delay { milliseconds } => write!(f, "wait {milliseconds} ms"),
            Self::Unload { target } => write!(f, "unload, {target}"),
            Self::Load { target } => write!(f, "start loading, {target}"),
            Self::LoadCompleted { target } => write!(f, "loading completed, {target}"),
            Self::AbsSegment {
                target,
                address,
                size,
                ..
            } => write!(f, "segment {address:04X} of {size} bytes, {target}"),
            Self::TaskSegment { target, address } => {
                write!(f, "task segment {address:04X}, {target}")
            }
            Self::TaskPtr { target, .. } => write!(f, "task pointers, {target}"),
            Self::TaskCtrl1 { target, .. } => write!(f, "task control 1, {target}"),
            Self::TaskCtrl2 { target, .. } => write!(f, "task control 2, {target}"),
            Self::WriteProperty {
                object, property, ..
            } => write!(f, "write property {property}, {object}"),
            Self::CompareProperty {
                object, property, ..
            } => write!(f, "compare property {property}, {object}"),
            Self::LoadImageProperty {
                object, property, ..
            } => write!(f, "read property {property}, {object}"),
            Self::WriteMemory { address, size, .. } => {
                write!(f, "write memory {address:04X}, {size} bytes")
            }
            Self::SetControlVariable { variable, value } => {
                write!(f, "set {variable:?} to {value}")
            }
            Self::Unsupported { element } => write!(f, "{element} (no rules for it)"),
        }
    }
}

impl Action {
    /// The part of a download the step belongs to, in words a person follows progress by:
    /// "Address table", "Parameters", "Restart". `None` for steps that send nothing of their
    /// own.
    pub fn phase(&self) -> Option<&'static str> {
        let by_target = |target: &Target| match target {
            Target::Index(1) | Target::ObjectType { object_type: 1, .. } => "Address table",
            Target::Index(2) | Target::ObjectType { object_type: 2, .. } => "Association table",
            Target::Index(3) | Target::ObjectType { object_type: 3, .. } => {
                "Application and parameters"
            }
            Target::Index(4) | Target::ObjectType { object_type: 4, .. } => "Interface program",
            _ => "Device settings",
        };
        match self {
            Self::Connect
            | Self::Disconnect
            | Self::Delay { .. }
            | Self::SetControlVariable { .. } => None,
            Self::Restart => Some("Restart"),
            Self::Unload { .. } => Some("Unloading"),
            Self::WriteMemory { .. } => Some("Parameters"),
            Self::WriteProperty { .. }
            | Self::CompareProperty { .. }
            | Self::LoadImageProperty { .. }
            | Self::Unsupported { .. } => Some("Device settings"),
            Self::Load { target }
            | Self::LoadCompleted { target }
            | Self::AbsSegment { target, .. }
            | Self::TaskSegment { target, .. }
            | Self::TaskPtr { target, .. }
            | Self::TaskCtrl1 { target, .. }
            | Self::TaskCtrl2 { target, .. } => Some(by_target(target)),
        }
    }

    /// Whether the step needs the device. A missing connect is added before the first such
    /// step.
    pub fn needs_device(&self) -> bool {
        !matches!(self, Self::Delay { .. } | Self::SetControlVariable { .. })
    }

    /// Whether the step is an event for a load state machine.
    pub fn is_load_step(&self) -> bool {
        matches!(
            self,
            Self::Unload { .. }
                | Self::Load { .. }
                | Self::LoadCompleted { .. }
                | Self::AbsSegment { .. }
                | Self::TaskSegment { .. }
                | Self::TaskPtr { .. }
                | Self::TaskCtrl1 { .. }
                | Self::TaskCtrl2 { .. }
        )
    }

    /// The load state machine or object the step addresses, if any.
    pub fn target(&self) -> Option<Target> {
        match self {
            Self::Unload { target }
            | Self::Load { target }
            | Self::LoadCompleted { target }
            | Self::AbsSegment { target, .. }
            | Self::TaskSegment { target, .. }
            | Self::TaskPtr { target, .. }
            | Self::TaskCtrl1 { target, .. }
            | Self::TaskCtrl2 { target, .. } => Some(*target),
            Self::WriteProperty { object, .. }
            | Self::CompareProperty { object, .. }
            | Self::LoadImageProperty { object, .. } => Some(*object),
            _ => None,
        }
    }
}

/// One step of a load procedure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadStep {
    /// The download kinds the step belongs to.
    pub applies_to: AppliesTo,
    /// What the step does.
    pub action: Action,
    /// Error causes after which the procedure carries on.
    pub ignore: Vec<ErrorCause>,
}

impl LoadStep {
    /// A step that applies to every kind of download and ignores no errors.
    pub fn new(action: Action) -> Self {
        Self {
            applies_to: AppliesTo::All,
            action,
            ignore: Vec::new(),
        }
    }

    /// Read one `LdCtrl…` element.
    pub(crate) fn parse(element: &Element<'_>) -> Result<Self, Error> {
        let applies_to = AppliesTo::parse(element.attr("AppliesTo"))?;
        let mut ignore = Vec::new();
        for handler in element.children_named("OnError") {
            if !handler.attr("Ignore").is_some_and(parse_flag) {
                continue;
            }
            match handler.attr("Cause") {
                Some("ResourceNotFound") => ignore.push(ErrorCause::ResourceNotFound),
                Some("CompareMismatch") => ignore.push(ErrorCause::CompareMismatch),
                other => return Err(invalid("OnError Cause", other.unwrap_or_default())),
            }
        }
        let action = parse_action(element)?;
        Ok(Self {
            applies_to,
            action,
            ignore,
        })
    }
}

fn parse_flag(text: &str) -> bool {
    matches!(text, "true" | "1")
}

fn number<T: std::str::FromStr>(
    element: &Element<'_>,
    name: &'static str,
) -> Result<Option<T>, Error> {
    element
        .attr(name)
        .map(|text| text.trim().parse::<T>().map_err(|_| invalid(name, text)))
        .transpose()
}

fn required<T: std::str::FromStr>(element: &Element<'_>, name: &'static str) -> Result<T, Error> {
    number(element, name)?.ok_or_else(|| Error::Missing(format!("{name} on {}", element.name)))
}

fn hex_bytes(text: &str, what: &'static str) -> Result<Vec<u8>, Error> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err(invalid(what, text));
    }
    (0..text.len())
        .step_by(2)
        .map(|at| {
            text.get(at..at + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| invalid(what, text))
        })
        .collect()
}

fn target(element: &Element<'_>, index: &'static str) -> Result<Target, Error> {
    if let Some(object_type) = number::<u16>(element, "ObjType")? {
        let occurrence = number(element, "Occurrence")?.unwrap_or(0);
        return Ok(Target::ObjectType {
            object_type,
            occurrence,
        });
    }
    Ok(Target::Index(required(element, index)?))
}

fn parse_action(element: &Element<'_>) -> Result<Action, Error> {
    let lsm = || target(element, "LsmIdx");
    let inline = |name: &'static str| {
        element
            .attr(name)
            .map(|text| hex_bytes(text, name))
            .transpose()
    };
    let unsupported = || Action::Unsupported {
        element: element.name.to_string(),
    };
    Ok(match element.name {
        "LdCtrlConnect" => Action::Connect,
        "LdCtrlDisconnect" => Action::Disconnect,
        "LdCtrlRestart" => Action::Restart,
        "LdCtrlDelay" => Action::Delay {
            milliseconds: required(element, "MilliSeconds")?,
        },
        "LdCtrlUnload" => Action::Unload { target: lsm()? },
        "LdCtrlLoad" => Action::Load { target: lsm()? },
        "LdCtrlLoadCompleted" => Action::LoadCompleted { target: lsm()? },
        "LdCtrlAbsSegment" => Action::AbsSegment {
            target: lsm()?,
            segment_type: required(element, "SegType")?,
            address: required(element, "Address")?,
            size: required(element, "Size")?,
            access: required(element, "Access")?,
            memory_type: required(element, "MemType")?,
            flags: required(element, "SegFlags")?,
        },
        "LdCtrlTaskSegment" => Action::TaskSegment {
            target: lsm()?,
            address: required(element, "Address")?,
        },
        "LdCtrlTaskPtr" => Action::TaskPtr {
            target: lsm()?,
            init: required(element, "InitPtr")?,
            save: required(element, "SavePtr")?,
            serial: required(element, "SerialPtr")?,
        },
        "LdCtrlTaskCtrl1" => Action::TaskCtrl1 {
            target: lsm()?,
            address: required(element, "Address")?,
            count: required(element, "Count")?,
        },
        "LdCtrlTaskCtrl2" => Action::TaskCtrl2 {
            target: lsm()?,
            callback: required(element, "Callback")?,
            address: required(element, "Address")?,
            segment0: required(element, "Seg0")?,
            segment1: required(element, "Seg1")?,
        },
        "LdCtrlWriteProp" => Action::WriteProperty {
            object: target(element, "ObjIdx")?,
            property: required(element, "PropId")?,
            start: number(element, "StartElement")?.unwrap_or(1),
            count: number(element, "Count")?.unwrap_or(1),
            verify: element.attr("Verify").is_some_and(parse_flag),
            inline: inline("InlineData")?,
        },
        "LdCtrlCompareProp" => {
            // Range comparisons and retries are not modelled.
            if element.attr("Range").is_some() {
                return Ok(unsupported());
            }
            Action::CompareProperty {
                object: target(element, "ObjIdx")?,
                property: required(element, "PropId")?,
                start: number(element, "StartElement")?.unwrap_or(1),
                count: number(element, "Count")?.unwrap_or(1),
                inline: inline("InlineData")?,
                mask: inline("Mask")?,
                invert: element.attr("Invert").is_some_and(parse_flag),
            }
        }
        "LdCtrlLoadImageProp" => Action::LoadImageProperty {
            object: target(element, "ObjIdx")?,
            property: required(element, "PropId")?,
            start: number(element, "StartElement")?.unwrap_or(1),
            count: number(element, "Count")?.unwrap_or(1),
        },
        "LdCtrlWriteMem" => {
            if element
                .attr("AddressSpace")
                .is_some_and(|space| space != "Standard")
            {
                return Ok(unsupported());
            }
            Action::WriteMemory {
                address: required(element, "Address")?,
                size: required(element, "Size")?,
                verify: element.attr("Verify").is_some_and(parse_flag),
                inline: inline("InlineData")?,
            }
        }
        "LdCtrlSetControlVariable" => {
            let variable = match element.attr("Name") {
                Some("EnableSegmentWrite") => ControlVariable::SegmentWrite,
                Some("EnableOptimisticWrite") => ControlVariable::OptimisticWrite,
                Some("EnableMemoryAutoVerify") => ControlVariable::MemoryAutoVerify,
                Some("EnableVerifyOnWriteDirect") => ControlVariable::VerifyOnWriteDirect,
                other => return Err(invalid("control variable", other.unwrap_or_default())),
            };
            Action::SetControlVariable {
                variable,
                value: element.attr("Value").is_some_and(parse_flag),
            }
        }
        _ => unsupported(),
    })
}

/// Load state machine index some products use to announce the group object table.
const OBJECT_TABLE_STATE_MACHINE: u8 = 5;
/// Load state machine index of the second application program on BIM M112.
const SECOND_APPLICATION_STATE_MACHINE: u8 = 4;

/// Application programs whose steps on the second application's load state machine are left
/// alone, by the start of their identifier. From behaviour notes; unverified.
const SECOND_APPLICATION_EXEMPT: [&str; 7] = [
    "M-0001_A-8018-04",
    "M-0001_A-8018-06",
    "M-0001_A-8018-08",
    "M-0001_A-8020-01",
    "M-0001_A-8020-02",
    "M-0001_A-8020-03",
    "M-0001_A-8023-11",
];

/// Drop the product's load state machine steps that do not belong on this device. It is the
/// first thing that happens to a product procedure, whatever the kind of download.
///
/// Only the eight load state machine steps (unload, load, load completed, absolute segment,
/// task segment, task pointer, task control 1 and 2) are looked at, and only when they name
/// their state machine by index.
///
/// 1. A *load* or *task segment* step on index 5 is dropped unless the mask has a load state
///    machine for the group object table ([`MaskProfile::object_table_load_control`]). Older
///    products announce the object table's address this way; devices of these masks have
///    nothing to tell it to. Two recorded downloads (masks `0021` and `0705`) have such steps
///    after their restart, and neither shows them on the bus.
/// 2. On BIM M112, index 4 belongs to the device's second application program. In the
///    procedure of the first one, every such step except an unload is dropped, unless
///    `application` is one of a few exempt programs. An unload on index 4 stays: it would only
///    go if the device had a second application, and this crate refuses those devices. From
///    behaviour notes; no recording exercises it.
///
/// The counterpart of rule 2 for the second application's own procedure does not arise here.
pub fn remove_foreign_steps(steps: &mut Vec<LoadStep>, profile: &MaskProfile, application: &str) {
    let exempt = SECOND_APPLICATION_EXEMPT
        .iter()
        .any(|prefix| application.starts_with(prefix));
    steps.retain(|step| {
        let (Some(Target::Index(index)), true) = (step.action.target(), step.action.is_load_step())
        else {
            return true;
        };
        let announces_object_table = index == OBJECT_TABLE_STATE_MACHINE
            && matches!(
                step.action,
                Action::Load { .. } | Action::TaskSegment { .. }
            )
            && !profile.object_table_load_control;
        let second_application = profile.is_bim_m112()
            && index == SECOND_APPLICATION_STATE_MACHINE
            && !matches!(step.action, Action::Unload { .. })
            && !exempt;
        !announces_object_table && !second_application
    });
}

/// Keep the steps that belong to a download of `kind`.
pub fn select(steps: &[LoadStep], kind: DownloadKind) -> Vec<LoadStep> {
    steps
        .iter()
        .filter(|step| step.applies_to.includes(kind))
        .cloned()
        .collect()
}

/// Make connects and disconnects consistent: drop a disconnect that is directly followed by a
/// connect (both go), drop a connect while connected and a disconnect while not, add a connect
/// before the first step that needs the device, collapse doubled restarts, and end with a
/// disconnect. A restart does not count as a disconnect here.
///
/// On the recorded procedures this changes nothing; the rules are taken from behaviour notes.
pub fn normalise(steps: &mut Vec<LoadStep>) {
    let mut connected = false;
    let mut at = 0;
    while at < steps.len() {
        let here = steps.get(at).map(|step| &step.action);
        let next = steps.get(at + 1).map(|step| &step.action);
        match here {
            Some(Action::Disconnect) if next == Some(&Action::Connect) => {
                steps.drain(at..at + 2);
                at = at.saturating_sub(1);
                continue;
            }
            Some(Action::Connect) if connected => {
                steps.remove(at);
                continue;
            }
            Some(Action::Connect) => connected = true,
            Some(Action::Disconnect) if !connected => {
                steps.remove(at);
                continue;
            }
            Some(Action::Disconnect) => connected = false,
            Some(action) if !connected && action.needs_device() => {
                steps.insert(at, LoadStep::new(Action::Connect));
                connected = true;
            }
            _ => {}
        }
        let here = steps.get(at).map(|step| &step.action);
        let next = steps.get(at + 1).map(|step| &step.action);
        if here == Some(&Action::Restart) && next == Some(&Action::Restart) {
            steps.remove(at);
            continue;
        }
        at += 1;
    }
    if connected {
        steps.push(LoadStep::new(Action::Disconnect));
    }
}

/// Where the tables of a product with dynamic table management end up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TablePlacement {
    /// Bytes the address table occupies, including the extra byte some masks append.
    pub address_table_size: u16,
    /// New start of the association table, directly after the address table.
    pub association_table: u16,
}

/// Rewrite the table segments for dynamic table management: the address table segment (load
/// state machine 1) shrinks to what the table uses; the association table segment (load state
/// machine 2) starts right after it and keeps its old end; the association table's task
/// segment follows.
///
/// Verified against two recorded downloads.
pub fn patch_table_placement(steps: &mut [LoadStep], placement: TablePlacement) {
    for step in steps {
        match &mut step.action {
            Action::AbsSegment {
                target: Target::Index(1),
                size,
                ..
            } => *size = placement.address_table_size,
            Action::AbsSegment {
                target: Target::Index(2),
                address,
                size,
                ..
            } => {
                let end = u32::from(*address) + u32::from(*size);
                *address = placement.association_table;
                *size = u16::try_from(end.saturating_sub(u32::from(*address))).unwrap_or(u16::MAX);
            }
            Action::TaskSegment {
                target: Target::Index(2),
                address,
            } => *address = placement.association_table,
            _ => {}
        }
    }
}

/// Rewrite the steps marked [`AppliesTo::Auto`] for a download that is not a full one.
///
/// What happens to a step depends on the type of the interface object it targets (1 address
/// table, 2 association table, 3 application program, 4 interface program, 9 group object
/// table); a step whose target has no known type stays as it is.
///
/// | Step | Kept when |
/// |---|---|
/// | unload | tables (1, 2): tables are downloaded; 3, 4, 9: never |
/// | load, load completed | 1, 2, 9: tables are downloaded; 4: never; else always |
/// | absolute segment | 1, 2, 9: tables are downloaded; 3: replaced by a verified memory write of the same range if it is an EEPROM segment, else dropped; 4: never |
/// | task segment | 1, 2, 9: tables are downloaded; 3, 4: never |
/// | task pointer, task control 1 and 2 | never |
///
/// Taken from behaviour notes. One recorded partial download (mask `0705`) is reproduced by
/// these rules up to its restart; nothing else is verified.
pub fn transform_for_partial(steps: &mut Vec<LoadStep>, kind: DownloadKind, profile: &MaskProfile) {
    if kind.full {
        return;
    }
    let tables = kind.group_communication;
    let mut segment_write = true;
    let mut out = Vec::with_capacity(steps.len());
    for mut step in steps.drain(..) {
        if let Action::SetControlVariable {
            variable: ControlVariable::SegmentWrite,
            value,
        } = step.action
        {
            segment_write = value;
        }
        if step.applies_to != AppliesTo::Auto {
            out.push(step);
            continue;
        }
        let object_type = step.action.target().and_then(|target| match target {
            Target::Index(index) => profile.object_type(index),
            Target::ObjectType { object_type, .. } => Some(object_type),
        });
        let keep = match (&step.action, object_type) {
            (Action::TaskPtr { .. } | Action::TaskCtrl1 { .. } | Action::TaskCtrl2 { .. }, _) => {
                false
            }
            (_, None) => true,
            (Action::Unload { .. }, Some(1 | 2)) => tables,
            (Action::Unload { .. }, Some(3 | 4 | 9)) => false,
            (Action::Load { .. } | Action::LoadCompleted { .. }, Some(1 | 2 | 9)) => tables,
            (Action::Load { .. } | Action::LoadCompleted { .. }, Some(4)) => false,
            (Action::AbsSegment { .. } | Action::TaskSegment { .. }, Some(1 | 2 | 9)) => tables,
            (Action::TaskSegment { .. }, Some(3 | 4)) => false,
            (Action::AbsSegment { .. }, Some(4)) => false,
            (
                Action::AbsSegment {
                    address,
                    size,
                    memory_type,
                    ..
                },
                Some(3),
            ) => {
                if segment_write && memory_type & 3 == 3 {
                    step.action = Action::WriteMemory {
                        address: *address,
                        size: *size,
                        verify: true,
                        inline: None,
                    };
                    true
                } else {
                    false
                }
            }
            (Action::WriteProperty { property, .. }, Some(object_type)) => {
                // The table properties of property-based devices.
                let table_property = matches!((object_type, *property), (1 | 2, 23) | (9, 51 | 52));
                !table_property || tables
            }
            _ => true,
        };
        if keep {
            out.push(step);
        }
    }
    *steps = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    fn steps(doc: &str) -> Vec<LoadStep> {
        let root = xml::parse(doc).unwrap();
        root.children
            .iter()
            .map(|e| LoadStep::parse(e).unwrap())
            .collect()
    }

    fn actions(steps: &[LoadStep]) -> Vec<&Action> {
        steps.iter().map(|s| &s.action).collect()
    }

    #[test]
    fn parses_attributes() {
        let parsed = steps(
            r#"<P>
              <LdCtrlAbsSegment LsmIdx="1" SegType="0" Address="278" Size="176" Access="49" MemType="3" SegFlags="128" />
              <LdCtrlWriteProp ObjIdx="0" PropId="8" Verify="false" InlineData="00ff" AppliesTo="full" />
              <LdCtrlCompareProp ObjIdx="0" PropId="78" InlineData="0102"><OnError Cause="CompareMismatch" Ignore="true"/></LdCtrlCompareProp>
              <LdCtrlLoad ObjType="3" Occurrence="1" />
              <LdCtrlRelSegment LsmIdx="4" Size="10" Mode="0" Fill="0" />
            </P>"#,
        );
        assert_eq!(
            parsed[0].action,
            Action::AbsSegment {
                target: Target::Index(1),
                segment_type: 0,
                address: 278,
                size: 176,
                access: 49,
                memory_type: 3,
                flags: 128
            }
        );
        assert_eq!(parsed[0].applies_to, AppliesTo::Auto);
        assert_eq!(parsed[1].applies_to, AppliesTo::Full);
        assert_eq!(
            parsed[1].action,
            Action::WriteProperty {
                object: Target::Index(0),
                property: 8,
                start: 1,
                count: 1,
                verify: false,
                inline: Some(vec![0x00, 0xFF])
            }
        );
        assert_eq!(parsed[2].ignore, [ErrorCause::CompareMismatch]);
        assert_eq!(
            parsed[3].action,
            Action::Load {
                target: Target::ObjectType {
                    object_type: 3,
                    occurrence: 1
                }
            }
        );
        assert_eq!(
            parsed[4].action,
            Action::Unsupported {
                element: "LdCtrlRelSegment".into()
            }
        );
    }

    #[test]
    fn rejects_bad_attributes() {
        for doc in [
            r#"<P><LdCtrlLoad /></P>"#,
            r#"<P><LdCtrlLoad LsmIdx="300" /></P>"#,
            r#"<P><LdCtrlLoad LsmIdx="1" AppliesTo="sometimes" /></P>"#,
            r#"<P><LdCtrlWriteProp ObjIdx="0" PropId="8" InlineData="0g" /></P>"#,
            r#"<P><LdCtrlWriteProp ObjIdx="0" PropId="8" InlineData="000" /></P>"#,
        ] {
            let root = xml::parse(doc).unwrap();
            assert!(LoadStep::parse(&root.children[0]).is_err(), "{doc}");
        }
    }

    #[test]
    fn applies_to_filter() {
        let full = DownloadKind::FULL;
        let partial = DownloadKind::PARTIAL;
        let parameters_only = DownloadKind {
            group_communication: false,
            ..partial
        };
        assert!(AppliesTo::Auto.includes(full) && AppliesTo::All.includes(partial));
        assert!(AppliesTo::Full.includes(full) && !AppliesTo::Full.includes(partial));
        assert!(!AppliesTo::Parameters.includes(full) && AppliesTo::Parameters.includes(partial));
        assert!(!AppliesTo::Group.includes(parameters_only));
        assert!(AppliesTo::FullOrGroup.includes(full));
        assert!(!AppliesTo::FullOrGroup.includes(parameters_only));
        assert!(AppliesTo::ParametersAndGroup.includes(partial));
        assert!(!AppliesTo::ParametersAndGroup.includes(parameters_only));
    }

    #[test]
    fn foreign_steps_are_removed() {
        let doc = r#"<P>
              <LdCtrlLoad LsmIdx="3" />
              <LdCtrlRestart />
              <LdCtrlTaskSegment LsmIdx="5" Address="628" />
              <LdCtrlLoad LsmIdx="5" Occurrence="0" />
              <LdCtrlLoadCompleted LsmIdx="5" />
              <LdCtrlLoad ObjType="9" />
              <LdCtrlWriteProp ObjIdx="5" PropId="8" InlineData="0000" />
              <LdCtrlUnload LsmIdx="4" />
              <LdCtrlLoad LsmIdx="4" />
              <LdCtrlAbsSegment LsmIdx="4" SegType="0" Address="1" Size="1" Access="0" MemType="3" SegFlags="0" />
            </P>"#;
        let kept = |mask: u16, application: &str| -> Vec<String> {
            let mut list = steps(doc);
            let profile = MaskProfile::for_mask(mask).unwrap();
            remove_foreign_steps(&mut list, &profile, application);
            list.iter()
                .map(|step| match &step.action {
                    Action::Load {
                        target: Target::Index(index),
                    } => format!("load{index}"),
                    Action::Load { .. } => "load by type".into(),
                    Action::Unload { .. } => "unload4".into(),
                    Action::LoadCompleted { .. } => "completed5".into(),
                    Action::TaskSegment { .. } => "task5".into(),
                    Action::AbsSegment { .. } => "abs4".into(),
                    Action::Restart => "restart".into(),
                    Action::WriteProperty { .. } => "write".into(),
                    other => format!("{other:?}"),
                })
                .collect()
        };
        // BCU 2: only the announcement on index 5 goes.
        assert_eq!(
            kept(0x0021, "M-00FA_A-0001-01-0000"),
            [
                "load3",
                "restart",
                "completed5",
                "load by type",
                "write",
                "unload4",
                "load4",
                "abs4"
            ]
        );
        // BIM M112: index 4 goes too, apart from the unload.
        let m112 = [
            "load3",
            "restart",
            "completed5",
            "load by type",
            "write",
            "unload4",
        ];
        assert_eq!(kept(0x0705, "M-00FA_A-0001-01-0000"), m112);
        assert_eq!(kept(0x0701, "M-00FA_A-0001-01-0000"), m112);
        // An exempt program keeps its steps on index 4, but not the announcement.
        assert_eq!(
            kept(0x0701, "M-0001_A-8020-02-1234"),
            [
                "load3",
                "restart",
                "completed5",
                "load by type",
                "write",
                "unload4",
                "load4",
                "abs4"
            ]
        );
    }

    #[test]
    fn normaliser_adds_and_removes() {
        let load = Action::Load {
            target: Target::Index(1),
        };
        let mut list: Vec<LoadStep> = [
            Action::Delay { milliseconds: 5 },
            load.clone(),
            Action::Connect,
            Action::Disconnect,
            Action::Connect,
            Action::Restart,
            Action::Restart,
            Action::Disconnect,
            Action::Disconnect,
            load.clone(),
        ]
        .into_iter()
        .map(LoadStep::new)
        .collect();
        normalise(&mut list);
        assert_eq!(
            actions(&list),
            [
                &Action::Delay { milliseconds: 5 },
                &Action::Connect,
                &load,
                &Action::Restart,
                &Action::Disconnect,
                &Action::Connect,
                &load,
                &Action::Disconnect,
            ]
        );
        let mut empty = Vec::new();
        normalise(&mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn table_placement_patch() {
        let mut list = steps(
            r#"<P>
              <LdCtrlAbsSegment LsmIdx="1" SegType="0" Address="1000" Size="100" Access="1" MemType="3" SegFlags="128" />
              <LdCtrlTaskSegment LsmIdx="1" Address="1000" />
              <LdCtrlAbsSegment LsmIdx="2" SegType="0" Address="1100" Size="50" Access="1" MemType="3" SegFlags="128" />
              <LdCtrlTaskSegment LsmIdx="2" Address="1100" />
              <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="2000" Size="10" Access="1" MemType="3" SegFlags="0" />
            </P>"#,
        );
        let untouched = list[4].clone();
        patch_table_placement(
            &mut list,
            TablePlacement {
                address_table_size: 8,
                association_table: 1008,
            },
        );
        assert!(matches!(
            list[0].action,
            Action::AbsSegment {
                address: 1000,
                size: 8,
                ..
            }
        ));
        assert!(matches!(
            list[1].action,
            Action::TaskSegment { address: 1000, .. }
        ));
        assert!(matches!(
            list[2].action,
            Action::AbsSegment {
                address: 1008,
                size: 142,
                ..
            }
        ));
        assert!(matches!(
            list[3].action,
            Action::TaskSegment { address: 1008, .. }
        ));
        assert_eq!(list[4], untouched);
    }

    #[test]
    fn partial_transform() {
        let doc = r#"<P>
              <LdCtrlUnload LsmIdx="1" /><LdCtrlUnload LsmIdx="3" />
              <LdCtrlLoad LsmIdx="1" />
              <LdCtrlAbsSegment LsmIdx="1" SegType="0" Address="1000" Size="100" Access="1" MemType="3" SegFlags="128" />
              <LdCtrlTaskSegment LsmIdx="1" Address="1000" />
              <LdCtrlLoadCompleted LsmIdx="1" />
              <LdCtrlLoad LsmIdx="3" />
              <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="500" Size="9" Access="1" MemType="2" SegFlags="0" />
              <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="2000" Size="10" Access="1" MemType="3" SegFlags="0" />
              <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="2010" Size="4" Access="1" MemType="3" SegFlags="0" AppliesTo="all" />
              <LdCtrlTaskSegment LsmIdx="3" Address="2000" />
              <LdCtrlTaskPtr LsmIdx="3" InitPtr="1" SavePtr="2" SerialPtr="3" />
              <LdCtrlTaskCtrl1 LsmIdx="3" Address="0" Count="0" />
              <LdCtrlTaskCtrl2 LsmIdx="3" Callback="1" Address="2" Seg0="3" Seg1="4" />
              <LdCtrlLoadCompleted LsmIdx="3" />
              <LdCtrlLoad LsmIdx="4" />
              <LdCtrlTaskSegment LsmIdx="5" Address="2000" />
              <LdCtrlLoadImageProp ObjIdx="3" PropId="27" Count="2" />
            </P>"#;
        let profile = MaskProfile::for_mask(0x0705).unwrap();

        let mut full = steps(doc);
        let before = full.clone();
        transform_for_partial(&mut full, DownloadKind::FULL, &profile);
        assert_eq!(full, before);

        let mut partial = steps(doc);
        transform_for_partial(&mut partial, DownloadKind::PARTIAL, &profile);
        let names: Vec<String> = partial
            .iter()
            .map(|s| match &s.action {
                Action::Unload {
                    target: Target::Index(i),
                } => format!("unload{i}"),
                Action::Load {
                    target: Target::Index(i),
                } => format!("load{i}"),
                Action::LoadCompleted {
                    target: Target::Index(i),
                } => format!("completed{i}"),
                Action::AbsSegment { address, .. } => format!("abs@{address}"),
                Action::TaskSegment { address, .. } => format!("task@{address}"),
                Action::WriteMemory { address, size, .. } => format!("write@{address}+{size}"),
                Action::LoadImageProperty { .. } => "loadimage".into(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            names,
            [
                "unload1",
                "load1",
                "abs@1000",
                "task@1000",
                "completed1",
                "load3",
                "write@2000+10",
                "abs@2010",
                "completed3",
                "task@2000",
                "loadimage",
            ]
        );

        let mut parameters_only = steps(doc);
        let kind = DownloadKind {
            group_communication: false,
            ..DownloadKind::PARTIAL
        };
        transform_for_partial(&mut parameters_only, kind, &profile);
        assert_eq!(parameters_only.len(), 6);
    }
}
