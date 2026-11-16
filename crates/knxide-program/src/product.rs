//! The application program of a product: what ETS reads from a `.knxprod` or from the product
//! copy inside a `.knxproj` (one XML file per application program).
//!
//! Identifiers are stored without the application program's own identifier in front, so
//! `M-0001_A-0002-03-0000_P-7_R-7` becomes `P-7_R-7`. Project files use both spellings.

use std::collections::HashMap;

use crate::base64;
use crate::error::{Error, invalid};
use crate::procedure::LoadStep;
use crate::xml::{self, Element};

/// How the load procedure of a product is put together (attribute `LoadProcedureStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadProcedureStyle {
    /// The product lists every step itself. The only style this crate plans.
    Product,
    /// The mask's default procedure is used.
    Default,
    /// The mask's default procedure with product steps merged in.
    Merged,
}

/// Switches from the `Options` element that matter for a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProgramOptions {
    /// `PreferPartialDownloadIfApplicationLoaded`.
    pub prefer_partial_download: bool,
    /// `LegacyNoMemoryVerifyMode`: never use the device's echo mode for memory writes.
    pub no_memory_verify_mode: bool,
    /// `LegacyNoOptimisticWrite`: never read before writing.
    pub no_optimistic_write: bool,
    /// `LegacyDoNotReportPropertyWriteErrors`. Parsed; a download with it set is refused.
    pub ignore_property_write_errors: bool,
    /// `ParameterByteOrder` is `LittleEndian`. Parsed; a download with it set is refused.
    pub little_endian_parameters: bool,
    /// `TextParameterZeroTerminate`: text parameters keep their last byte zero.
    pub zero_terminate_text: bool,
    /// What happens to parameters that are not visible.
    pub invisible_parameters: InvisibleParameters,
    /// `LegacyDoNotCheckManufacturerId`: the device's manufacturer is not compared before a
    /// download.
    pub skip_manufacturer_check: bool,
    /// `LegacyAllowPartialDownloadIfAp2Mismatch`: a partial download does not look at the
    /// device's second application program.
    pub ignore_second_application: bool,
    /// `PromptBeforeFullDownload`: the product wants the user asked before a partial download
    /// turns into a full one. Parsed only: this crate has nobody to ask, and a caller that
    /// does can look here.
    pub prompt_before_full_download: bool,
    /// `LegacyPatchManufacturerIdInTaskSegment`. Parsed; a download with it set is refused.
    /// The attribute name is taken from behaviour notes and not seen in a product file.
    pub patch_manufacturer_id: bool,
}

/// Treatment of parameters that the current configuration hides (`DownloadInvisibleParameters`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InvisibleParameters {
    /// Their bytes are sent with the content the product image has. The default, and the only
    /// behaviour verified against recorded downloads.
    #[default]
    Background,
    /// Their default value is written. Parsed; a download with it set is refused.
    DefaultValue,
    /// Their bytes are left alone.
    None,
}

/// A block of device memory with the product's initial content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Identifier, as referenced by parameters and tables.
    pub id: String,
    /// Absolute start address.
    pub address: u32,
    /// Length in bytes.
    pub size: u32,
    /// Initial content. May be absent or shorter than `size`.
    pub data: Option<Vec<u8>>,
    /// Per-byte validity of `data`; see [`crate::Image`] for its meaning.
    pub mask: Option<Vec<u8>>,
}

/// One member of an enumeration type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumMember {
    /// The value as parameters store it.
    pub value: String,
    /// What the member is called.
    pub text: String,
    /// Position among the members as the product wants them shown.
    pub order: i64,
}

/// Kind and size of a parameter value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterType {
    /// Identifier.
    pub id: String,
    /// The value kind.
    pub kind: ParameterKind,
    /// Size of the stored value in bits; 0 when the type stores nothing.
    pub size_in_bit: u32,
    /// Smallest value of a number type (`minInclusive`), where the product states one.
    pub minimum: Option<i64>,
    /// Largest value of a number type (`maxInclusive`), where the product states one.
    pub maximum: Option<i64>,
    /// The members of an enumeration type, in the order the product wants them shown.
    pub members: Vec<EnumMember>,
}

/// Who may see and change a parameter (attribute `Access`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    /// Shown and editable. The default.
    #[default]
    ReadWrite,
    /// Shown, not editable.
    Read,
    /// Never shown.
    None,
}

impl Access {
    fn parse(text: Option<&str>) -> Result<Option<Self>, Error> {
        Ok(match text {
            None => None,
            Some("ReadWrite") => Some(Self::ReadWrite),
            Some("Read") => Some(Self::Read),
            Some("None") => Some(Self::None),
            Some(other) => return Err(invalid("Access", other)),
        })
    }
}

/// The kinds of parameter value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParameterKind {
    /// Integer (`TypeNumber`).
    Number {
        /// Whether negative values are allowed (stored in two's complement).
        signed: bool,
    },
    /// Enumeration whose value is stored as an unsigned integer (`TypeRestriction`).
    Restriction,
    /// Text (`TypeText`).
    Text,
    /// Occupies no memory (`TypeNone`).
    None,
    /// Enumeration whose members are byte strings (`TypeRestriction` with
    /// `Base="BinaryValue"`): each entry is a member's `Value` and its bytes. Seen for a
    /// hardware identification kept in a property; not encoded into memory.
    Binary(Vec<(String, Vec<u8>)>),
    /// A kind this crate cannot encode. The string is the element name.
    Other(String),
}

/// Where a parameter's value lives in the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParameterLocation {
    /// Nowhere: the parameter only steers visibility.
    None,
    /// In a memory segment.
    Memory {
        /// Identifier of the segment.
        segment: String,
        /// Position of the value's first bit, counted from the most significant bit of the
        /// segment's first byte. For a union member this already includes the union's position.
        bit: u32,
    },
    /// In an interface object property. Such parameters are not part of the memory image and
    /// this crate does not download them; a hardware identification kept this way is compared
    /// with the device before a download.
    Property {
        /// Object index, when the property is addressed by index (`ObjectIndex`); `None` when
        /// it is addressed by object type.
        object: Option<u8>,
        /// Property identifier.
        property: u16,
        /// Position of the value's first bit inside the property value.
        bit: u32,
    },
}

/// A parameter definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// Identifier.
    pub id: String,
    /// Identifier of its [`ParameterType`].
    pub parameter_type: String,
    /// Default value.
    pub value: String,
    /// Where the value is stored.
    pub location: ParameterLocation,
    /// What the parameter is called on the parameter pages (`Text`); may be empty.
    pub text: String,
    /// The product's own name for it (`Name`), not meant for display; may be empty.
    pub name: String,
    /// What follows the value, a unit for instance (`SuffixText`); may be empty.
    pub suffix: String,
    /// Who may see and change it.
    pub access: Access,
}

/// A use of a parameter; the unit that is shown, hidden and given a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterRef {
    /// Identifier.
    pub id: String,
    /// Identifier of the [`Parameter`].
    pub parameter: String,
    /// Default value that replaces the parameter's own, if given.
    pub value: Option<String>,
    /// A label that replaces the parameter's own, if given.
    pub text: Option<String>,
    /// An access rule that replaces the parameter's own, if given.
    pub access: Option<Access>,
    /// Where the reference stands among all of the product's (`DisplayOrder`), if stated.
    /// Products made for older tools order their parameters and pages with it.
    pub display_order: Option<u32>,
}

/// Transmission priority of a group object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectPriority {
    /// Low, the default.
    Low,
    /// High.
    High,
    /// Alert.
    Alert,
}

/// Attributes of a group object. In a reference, `None` means "as the object says".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjectAttributes {
    /// Value size in bits.
    pub size_in_bit: Option<u16>,
    /// Transmission priority.
    pub priority: Option<ObjectPriority>,
    /// Communication flag.
    pub communication: Option<bool>,
    /// Read flag.
    pub read: Option<bool>,
    /// Write flag.
    pub write: Option<bool>,
    /// Transmit flag.
    pub transmit: Option<bool>,
    /// Update flag.
    pub update: Option<bool>,
    /// Read-on-init flag.
    pub read_on_init: Option<bool>,
}

impl ObjectAttributes {
    pub(crate) fn parse(element: &Element<'_>) -> Result<Self, Error> {
        let flag = |name: &'static str| match element.attr(name) {
            None => Ok(None),
            Some("Enabled") => Ok(Some(true)),
            Some("Disabled") => Ok(Some(false)),
            Some(other) => Err(invalid(name, other)),
        };
        let priority = match element.attr("Priority") {
            None => None,
            Some("Low") => Some(ObjectPriority::Low),
            Some("High") => Some(ObjectPriority::High),
            Some("Alert") => Some(ObjectPriority::Alert),
            Some(other) => return Err(invalid("Priority", other)),
        };
        Ok(Self {
            size_in_bit: element
                .attr("ObjectSize")
                .map(parse_object_size)
                .transpose()?,
            priority,
            communication: flag("CommunicationFlag")?,
            read: flag("ReadFlag")?,
            write: flag("WriteFlag")?,
            transmit: flag("TransmitFlag")?,
            update: flag("UpdateFlag")?,
            read_on_init: flag("ReadOnInitFlag")?,
        })
    }

    /// These attributes, with gaps filled from `base`.
    pub fn or(self, base: Self) -> Self {
        Self {
            size_in_bit: self.size_in_bit.or(base.size_in_bit),
            priority: self.priority.or(base.priority),
            communication: self.communication.or(base.communication),
            read: self.read.or(base.read),
            write: self.write.or(base.write),
            transmit: self.transmit.or(base.transmit),
            update: self.update.or(base.update),
            read_on_init: self.read_on_init.or(base.read_on_init),
        }
    }
}

/// `"1 Bit"`, `"2 Bytes"` and so on, in bits.
fn parse_object_size(text: &str) -> Result<u16, Error> {
    let bad = || invalid("ObjectSize", text);
    let (count, unit) = text.trim().split_once(' ').ok_or_else(bad)?;
    let count: u16 = count.parse().map_err(|_| bad())?;
    match unit.trim() {
        "Bit" | "Bits" => Ok(count),
        "Byte" | "Bytes" => count.checked_mul(8).ok_or_else(bad),
        _ => Err(bad()),
    }
}

/// A group object definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComObject {
    /// Identifier.
    pub id: String,
    /// Group object number: its position in the device's group object table.
    pub number: u16,
    /// Its attributes.
    pub attributes: ObjectAttributes,
    /// What the object is called (`Text`); may be empty.
    pub text: String,
    /// What the object does (`FunctionText`); may be empty.
    pub function_text: String,
    /// Datapoint types the product names for the object (`DatapointType`, for example
    /// `DPST-1-1`); empty when it names none.
    pub datapoint_type: String,
}

/// A use of a group object, possibly with changed attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComObjectRef {
    /// Identifier.
    pub id: String,
    /// Identifier of the [`ComObject`].
    pub object: String,
    /// Attributes that replace the object's own.
    pub overrides: ObjectAttributes,
    /// A name that replaces the object's own (`Text`).
    pub text: Option<String>,
    /// A function description that replaces the object's own (`FunctionText`).
    pub function_text: Option<String>,
    /// Datapoint types that replace the object's own (`DatapointType`).
    pub datapoint_type: Option<String>,
}

/// Where a communication table starts and how much room it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableLocation {
    /// Identifier of the segment that holds the table.
    pub segment: String,
    /// Offset of the table inside the segment.
    pub offset: u32,
    /// Capacity in entries, where the product states one.
    pub max_entries: Option<u32>,
}

/// One node of the tree that decides which parameters and group objects are in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DynamicNode {
    /// A parameter reference is in use (`ParameterRefRef`).
    ParameterRef(String),
    /// A group object reference is in use (`ComObjectRefRef`).
    ComObjectRef(String),
    /// Branches chosen by the value of a parameter reference (`choose`).
    Choose {
        /// Identifier of the parameter reference whose value selects.
        parameter_ref: String,
        /// The branches, in document order.
        branches: Vec<When>,
    },
    /// A channel: a named group of parameter pages (`Channel`).
    Channel {
        /// What the channel is called; may be empty.
        text: String,
        /// What it contains.
        children: Vec<DynamicNode>,
    },
    /// A parameter page (`ParameterBlock`).
    Block {
        /// Identifier.
        id: String,
        /// What the page is called (`Text`, else `Name`). Where [`Self::Block::title`] names
        /// a parameter reference with a label, that label is the better title.
        text: String,
        /// The parameter reference whose label is the page's title (`ParamRefId`), for
        /// products that name their pages this way.
        title: Option<String>,
        /// Whether the page is shown.
        access: Access,
        /// What it contains.
        children: Vec<DynamicNode>,
    },
    /// A line of text between parameters (`ParameterSeparator`).
    Separator(String),
    /// Any other container.
    Group(Vec<DynamicNode>),
}

/// One branch of a [`DynamicNode::Choose`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct When {
    /// The test expression, if any.
    pub test: Option<String>,
    /// Whether this is the branch taken when no test matches.
    pub default: bool,
    /// What the branch contains.
    pub children: Vec<DynamicNode>,
}

/// An application program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationProgram {
    /// Identifier, for example `M-0001_A-0002-03-0000`.
    pub id: String,
    /// Display name (attribute `Name`); empty when absent.
    pub name: String,
    /// Identifier of a manufacturer plug-in that performs the download
    /// (`Static/Extension/@EtsDownloadPlugin`). This crate cannot stand in for it.
    pub download_plugin: Option<String>,
    /// Identifier of a manufacturer plug-in that replaces the parameter dialog
    /// (`Static/Extension/@EtsUiPlugin`). Such a plug-in may decide which parameters and group
    /// objects are in use, which this crate then gets wrong.
    pub ui_plugin: Option<String>,
    /// Whether the application is made for KNX Secure (`IsSecureEnabled`). This crate does not
    /// download such applications.
    pub secure: bool,
    /// Mask version the application is written for.
    pub mask_version: u16,
    /// Manufacturer identifier, from the `M-xxxx` part of the identifier.
    pub manufacturer: u16,
    /// Application number.
    pub application_number: u16,
    /// Application version.
    pub application_version: u8,
    /// Type of the physical external interface the application expects.
    pub pei_type: u8,
    /// How the load procedure is put together.
    pub load_procedure_style: LoadProcedureStyle,
    /// Whether the association table moves directly behind the address table.
    pub dynamic_table_management: bool,
    /// Download switches.
    pub options: ProgramOptions,
    /// Memory segments, in document order.
    pub segments: Vec<Segment>,
    /// Parameter types.
    pub parameter_types: Vec<ParameterType>,
    /// Parameters, union members included.
    pub parameters: Vec<Parameter>,
    /// Parameter references, in document order.
    pub parameter_refs: Vec<ParameterRef>,
    /// Group objects.
    pub com_objects: Vec<ComObject>,
    /// Group object references, in document order.
    pub com_object_refs: Vec<ComObjectRef>,
    /// Location of the group address table.
    pub address_table: Option<TableLocation>,
    /// Location of the association table.
    pub association_table: Option<TableLocation>,
    /// Location of the group object table.
    pub com_object_table: Option<TableLocation>,
    /// The product's own load procedure, as written.
    pub load_procedure: Vec<LoadStep>,
    /// The visibility tree.
    pub dynamic: Vec<DynamicNode>,
    index: Index,
}

/// Lookup tables from identifier to position.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Index {
    segments: HashMap<String, usize>,
    parameter_types: HashMap<String, usize>,
    parameters: HashMap<String, usize>,
    parameter_refs: HashMap<String, usize>,
    com_objects: HashMap<String, usize>,
    com_object_refs: HashMap<String, usize>,
}

fn build_index<T>(items: &[T], id: impl Fn(&T) -> &str) -> HashMap<String, usize> {
    items
        .iter()
        .enumerate()
        .map(|(at, item)| (id(item).to_string(), at))
        .collect()
}

pub(crate) fn parse_bool(text: &str, what: &'static str) -> Result<bool, Error> {
    match text.trim() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(invalid(what, other)),
    }
}

fn require<'e>(element: &'e Element<'_>, name: &'static str) -> Result<&'e str, Error> {
    element
        .attr(name)
        .ok_or_else(|| Error::Missing(format!("{name} on {}", element.name)))
}

fn decimal<T: std::str::FromStr>(element: &Element<'_>, name: &'static str) -> Result<T, Error> {
    let text = require(element, name)?;
    text.trim().parse().map_err(|_| invalid(name, text))
}

fn optional_decimal<T: std::str::FromStr>(
    element: &Element<'_>,
    name: &'static str,
) -> Result<Option<T>, Error> {
    element
        .attr(name)
        .map(|text| text.trim().parse().map_err(|_| invalid(name, text)))
        .transpose()
}

/// Texts of one language by element identifier and attribute name.
type Translations = HashMap<(String, String), String>;

/// Reads identifiers relative to one application program, and texts in the chosen language.
struct Ids<'p> {
    prefix: &'p str,
    texts: &'p Translations,
}

impl Ids<'_> {
    /// The text attribute `name` of `element`: its translation where the chosen language has
    /// one, else what the element itself says. `None` when neither exists.
    fn text_of(&self, element: &Element<'_>, name: &str) -> Option<String> {
        let translated = element.attr("Id").and_then(|id| {
            self.texts
                .get(&(self.local(id).to_string(), name.to_string()))
        });
        translated
            .cloned()
            .or_else(|| element.attr(name).map(str::to_string))
    }

    /// Like [`Self::text_of`], empty when there is no such text.
    fn text(&self, element: &Element<'_>, name: &str) -> String {
        self.text_of(element, name).unwrap_or_default()
    }

    /// Drop the application identifier and its separator from the front of `id`.
    fn local<'i>(&self, id: &'i str) -> &'i str {
        local_id(self.prefix, id)
    }

    fn of(&self, element: &Element<'_>, name: &'static str) -> Result<String, Error> {
        Ok(self.local(require(element, name)?).to_string())
    }
}

/// Drop `application` and the `_` after it from the front of `id`, if present.
pub(crate) fn local_id<'i>(application: &str, id: &'i str) -> &'i str {
    id.strip_prefix(application)
        .and_then(|rest| rest.strip_prefix('_'))
        .unwrap_or(id)
}

impl ApplicationProgram {
    /// Parse an application program XML file. The first `ApplicationProgram` element found is
    /// read.
    pub fn parse(text: &str) -> Result<Self, Error> {
        Self::parse_in(text, None)
    }

    /// Parse an application program XML file with its texts in `language` (an identifier
    /// such as `en-US`) where the file has them: the exact language, else one with the same
    /// first part (`en`), else the language the product was written in.
    pub fn parse_in(text: &str, language: Option<&str>) -> Result<Self, Error> {
        let root = xml::parse(text)?;
        let program = root
            .find("ApplicationProgram")
            .ok_or_else(|| Error::Missing("ApplicationProgram element".into()))?;
        let id = require(program, "Id")?;
        let texts = match (language, root.find("Languages")) {
            (Some(language), Some(languages))
                if program.attr("DefaultLanguage") != Some(language) =>
            {
                translations(languages, language, id)
            }
            _ => Translations::new(),
        };
        Self::from_element(program, &texts)
    }

    fn from_element(program: &Element<'_>, texts: &Translations) -> Result<Self, Error> {
        let id = require(program, "Id")?.to_string();
        let ids = Ids { prefix: &id, texts };
        let mask = require(program, "MaskVersion")?;
        let mask_version = mask
            .strip_prefix("MV-")
            .and_then(|rest| rest.get(..4))
            .and_then(|hex| u16::from_str_radix(hex, 16).ok())
            .ok_or_else(|| invalid("MaskVersion", mask))?;
        let manufacturer = id
            .strip_prefix("M-")
            .and_then(|rest| rest.get(..4))
            .and_then(|hex| u16::from_str_radix(hex, 16).ok())
            .ok_or_else(|| invalid("application program Id", &id))?;
        let load_procedure_style = match program.attr("LoadProcedureStyle") {
            Some("ProductProcedure") => LoadProcedureStyle::Product,
            Some("MergedProcedure") => LoadProcedureStyle::Merged,
            Some("DefaultProcedure") | None => LoadProcedureStyle::Default,
            Some(other) => return Err(invalid("LoadProcedureStyle", other)),
        };
        let dynamic_table_management = program
            .attr("DynamicTableManagement")
            .map(|text| parse_bool(text, "DynamicTableManagement"))
            .transpose()?
            .unwrap_or(false);
        let empty = Element::empty();
        let statics = program.child("Static").unwrap_or(&empty);

        let mut segments = Vec::new();
        for element in statics
            .child("Code")
            .into_iter()
            .flat_map(|code| code.children_named("AbsoluteSegment"))
        {
            segments.push(parse_segment(element, &ids)?);
        }

        let mut parameter_types = Vec::new();
        for element in statics
            .child("ParameterTypes")
            .into_iter()
            .flat_map(|types| types.children_named("ParameterType"))
        {
            parameter_types.push(parse_parameter_type(element, &ids)?);
        }

        let mut parameters = Vec::new();
        for element in statics
            .child("Parameters")
            .into_iter()
            .flat_map(|list| list.children.iter())
        {
            match element.name {
                "Parameter" => {
                    let location = parse_location(element, &ids, 0)?;
                    parameters.push(parse_parameter(element, &ids, location)?);
                }
                "Union" => {
                    for member in element.children_named("Parameter") {
                        let own = bit_position(
                            decimal(member, "Offset")?,
                            optional_decimal(member, "BitOffset")?.unwrap_or(0),
                            0,
                        )?;
                        let location = parse_location(element, &ids, own)?;
                        parameters.push(parse_parameter(member, &ids, location)?);
                    }
                }
                _ => {}
            }
        }

        let mut parameter_refs = Vec::new();
        for element in statics
            .child("ParameterRefs")
            .into_iter()
            .flat_map(|list| list.children_named("ParameterRef"))
        {
            parameter_refs.push(ParameterRef {
                id: ids.of(element, "Id")?,
                parameter: ids.of(element, "RefId")?,
                value: element.attr("Value").map(str::to_string),
                text: ids.text_of(element, "Text"),
                access: Access::parse(element.attr("Access"))?,
                display_order: optional_decimal(element, "DisplayOrder")?,
            });
        }

        let object_table = statics.child("ComObjectTable");
        let mut com_objects = Vec::new();
        for element in object_table
            .into_iter()
            .flat_map(|table| table.children_named("ComObject"))
        {
            com_objects.push(ComObject {
                id: ids.of(element, "Id")?,
                number: decimal(element, "Number")?,
                attributes: ObjectAttributes::parse(element)?,
                text: ids.text(element, "Text"),
                function_text: ids.text(element, "FunctionText"),
                datapoint_type: element
                    .attr("DatapointType")
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        let mut com_object_refs = Vec::new();
        for element in statics
            .child("ComObjectRefs")
            .into_iter()
            .flat_map(|list| list.children_named("ComObjectRef"))
        {
            com_object_refs.push(ComObjectRef {
                id: ids.of(element, "Id")?,
                object: ids.of(element, "RefId")?,
                overrides: ObjectAttributes::parse(element)?,
                text: ids.text_of(element, "Text"),
                function_text: ids.text_of(element, "FunctionText"),
                datapoint_type: element.attr("DatapointType").map(str::to_string),
            });
        }

        let table = |element: Option<&Element<'_>>| -> Result<Option<TableLocation>, Error> {
            let Some(element) = element else {
                return Ok(None);
            };
            if element.attr("CodeSegment").is_none() {
                return Ok(None);
            }
            Ok(Some(TableLocation {
                segment: ids.of(element, "CodeSegment")?,
                offset: optional_decimal(element, "Offset")?.unwrap_or(0),
                max_entries: optional_decimal(element, "MaxEntries")?,
            }))
        };

        let mut load_procedure = Vec::new();
        // Only the first procedure is read; further ones belong to merge points of mask
        // default procedures, which this crate does not plan.
        if let Some(procedure) = statics
            .child("LoadProcedures")
            .and_then(|list| list.child("LoadProcedure"))
        {
            for element in &procedure.children {
                load_procedure.push(LoadStep::parse(element)?);
            }
        }

        let dynamic = program
            .child("Dynamic")
            .map(|element| parse_dynamic(element, &ids))
            .transpose()?
            .unwrap_or_default();

        let options = parse_options(program, statics)?;
        let extension = statics.child("Extension");
        let plugin = |name: &str| {
            extension
                .and_then(|extension| extension.attr(name))
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        };
        let index = Index {
            segments: build_index(&segments, |s| &s.id),
            parameter_types: build_index(&parameter_types, |t| &t.id),
            parameters: build_index(&parameters, |p| &p.id),
            parameter_refs: build_index(&parameter_refs, |r| &r.id),
            com_objects: build_index(&com_objects, |o| &o.id),
            com_object_refs: build_index(&com_object_refs, |r| &r.id),
        };
        Ok(Self {
            name: ids.text(program, "Name"),
            secure: program
                .attr("IsSecureEnabled")
                .is_some_and(|text| matches!(text.trim(), "1" | "true")),
            download_plugin: plugin("EtsDownloadPlugin"),
            ui_plugin: plugin("EtsUiPlugin"),
            mask_version,
            manufacturer,
            application_number: decimal(program, "ApplicationNumber")?,
            application_version: decimal(program, "ApplicationVersion")?,
            pei_type: optional_decimal(program, "PeiType")?.unwrap_or(0),
            load_procedure_style,
            dynamic_table_management,
            options,
            segments,
            parameter_types,
            parameters,
            parameter_refs,
            com_objects,
            com_object_refs,
            address_table: table(statics.child("AddressTable"))?,
            association_table: table(statics.child("AssociationTable"))?,
            com_object_table: table(object_table)?,
            load_procedure,
            dynamic,
            index,
            id,
        })
    }

    /// An identifier as this program stores it: without the program's own identifier in front.
    pub fn local_id<'i>(&self, id: &'i str) -> &'i str {
        local_id(&self.id, id)
    }

    /// The segment called `id`.
    pub fn segment(&self, id: &str) -> Option<&Segment> {
        let at = self.index.segments.get(self.local_id(id))?;
        self.segments.get(*at)
    }

    /// The parameter type called `id`.
    pub fn parameter_type(&self, id: &str) -> Option<&ParameterType> {
        let at = self.index.parameter_types.get(self.local_id(id))?;
        self.parameter_types.get(*at)
    }

    /// The parameter called `id`.
    pub fn parameter(&self, id: &str) -> Option<&Parameter> {
        let at = self.index.parameters.get(self.local_id(id))?;
        self.parameters.get(*at)
    }

    /// Position of the parameter reference called `id` in [`Self::parameter_refs`].
    pub fn parameter_ref_index(&self, id: &str) -> Option<usize> {
        self.index.parameter_refs.get(self.local_id(id)).copied()
    }

    /// The group object called `id`.
    pub fn com_object(&self, id: &str) -> Option<&ComObject> {
        let at = self.index.com_objects.get(self.local_id(id))?;
        self.com_objects.get(*at)
    }

    /// Position of the group object reference called `id` in [`Self::com_object_refs`].
    pub fn com_object_ref_index(&self, id: &str) -> Option<usize> {
        self.index.com_object_refs.get(self.local_id(id)).copied()
    }

    /// Absolute address of a table.
    pub(crate) fn table_address(&self, table: &TableLocation) -> Result<u32, Error> {
        let segment = self
            .segment(&table.segment)
            .ok_or_else(|| Error::UnknownReference(table.segment.clone()))?;
        segment
            .address
            .checked_add(table.offset)
            .ok_or_else(|| Error::OutOfRange("table address".into()))
    }
}

fn parse_segment(element: &Element<'_>, ids: &Ids<'_>) -> Result<Segment, Error> {
    if element
        .attr("UserMemory")
        .map(|text| parse_bool(text, "UserMemory"))
        .transpose()?
        .unwrap_or(false)
    {
        return Err(Error::Unsupported("segments in user memory".into()));
    }
    let payload = |name: &str| {
        element
            .child(name)
            .map(|child| base64::decode(child.text()))
            .transpose()
    };
    Ok(Segment {
        id: ids.of(element, "Id")?,
        address: decimal(element, "Address")?,
        size: decimal(element, "Size")?,
        data: payload("Data")?,
        mask: payload("Mask")?,
    })
}

fn parse_parameter_type(element: &Element<'_>, ids: &Ids<'_>) -> Result<ParameterType, Error> {
    let id = ids.of(element, "Id")?;
    let Some(kind) = element.children.first() else {
        return Err(Error::Missing(format!("type definition in {id}")));
    };
    let size = optional_decimal::<u32>(kind, "SizeInBit")?;
    // Limits are whole numbers in the products seen; a limit written otherwise is no limit.
    let limit = |name: &str| {
        kind.attr(name)
            .and_then(|text| text.trim().parse::<i64>().ok())
    };
    let (minimum, maximum) = (limit("minInclusive"), limit("maxInclusive"));
    let mut members = Vec::new();
    if kind.name == "TypeRestriction" {
        for (position, member) in kind.children_named("Enumeration").enumerate() {
            let Some(value) = member.attr("Value") else {
                continue;
            };
            members.push(EnumMember {
                value: value.to_string(),
                text: ids.text(member, "Text"),
                order: member
                    .attr("DisplayOrder")
                    .and_then(|text| text.trim().parse().ok())
                    .unwrap_or(position as i64),
            });
        }
        members.sort_by_key(|member| member.order);
    }
    let (kind, size_in_bit) = match kind.name {
        "TypeNumber" => {
            let signed = match kind.attr("Type") {
                Some("signedInt") => true,
                Some("unsignedInt") | None => false,
                Some(other) => return Err(invalid("TypeNumber Type", other)),
            };
            (ParameterKind::Number { signed }, size.unwrap_or(0))
        }
        // An enumeration whose members carry byte strings instead of numbers.
        "TypeRestriction" if kind.attr("Base") == Some("BinaryValue") => {
            let mut members = Vec::new();
            for member in kind.children_named("Enumeration") {
                if let (Some(value), Some(bytes)) =
                    (member.attr("Value"), member.attr("BinaryValue"))
                {
                    members.push((value.to_string(), base64::decode(bytes)?));
                }
            }
            (ParameterKind::Binary(members), size.unwrap_or(0))
        }
        "TypeRestriction" => (ParameterKind::Restriction, size.unwrap_or(0)),
        "TypeText" => (ParameterKind::Text, size.unwrap_or(0)),
        "TypeNone" => (ParameterKind::None, 0),
        // Sizes of the remaining kinds follow other attributes; they are refused before any
        // of their bytes would be needed.
        other => (ParameterKind::Other(other.to_string()), size.unwrap_or(1)),
    };
    Ok(ParameterType {
        id,
        kind,
        size_in_bit,
        minimum,
        maximum,
        members,
    })
}

/// `offset` bytes plus `bit_offset` and `extra` bits, as one bit count.
fn bit_position(offset: u32, bit_offset: u32, extra: u32) -> Result<u32, Error> {
    offset
        .checked_mul(8)
        .and_then(|bits| bits.checked_add(bit_offset))
        .and_then(|bits| bits.checked_add(extra))
        .ok_or_else(|| Error::OutOfRange("parameter offset".into()))
}

/// Location from the `Memory` or `Property` child of `holder` (a parameter or a union), moved
/// by `extra` bits.
fn parse_location(
    holder: &Element<'_>,
    ids: &Ids<'_>,
    extra: u32,
) -> Result<ParameterLocation, Error> {
    if let Some(memory) = holder.child("Memory") {
        let bit = bit_position(
            decimal(memory, "Offset")?,
            optional_decimal(memory, "BitOffset")?.unwrap_or(0),
            extra,
        )?;
        return Ok(ParameterLocation::Memory {
            segment: ids.of(memory, "CodeSegment")?,
            bit,
        });
    }
    if let Some(property) = holder.child("Property") {
        let object = match property.attr("ObjectType") {
            Some(_) => None,
            None => optional_decimal(property, "ObjectIndex")?,
        };
        return Ok(ParameterLocation::Property {
            object,
            property: decimal(property, "PropertyId")?,
            bit: bit_position(
                optional_decimal(property, "Offset")?.unwrap_or(0),
                optional_decimal(property, "BitOffset")?.unwrap_or(0),
                extra,
            )?,
        });
    }
    Ok(ParameterLocation::None)
}

fn parse_parameter(
    element: &Element<'_>,
    ids: &Ids<'_>,
    location: ParameterLocation,
) -> Result<Parameter, Error> {
    Ok(Parameter {
        id: ids.of(element, "Id")?,
        parameter_type: ids.of(element, "ParameterType")?,
        value: element.attr("Value").unwrap_or_default().to_string(),
        location,
        text: ids.text(element, "Text"),
        name: element.attr("Name").unwrap_or_default().to_string(),
        suffix: ids.text(element, "SuffixText"),
        access: Access::parse(element.attr("Access"))?.unwrap_or_default(),
    })
}

/// The texts of `language` in a `Languages` element: the exact language if present, else the
/// first one that shares its first part. Keys are identifiers local to `application`.
fn translations(languages: &Element<'_>, language: &str, application: &str) -> Translations {
    let primary = |identifier: &str| {
        identifier
            .split('-')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let all = || languages.children_named("Language");
    let chosen = all()
        .find(|candidate| candidate.attr("Identifier") == Some(language))
        .or_else(|| {
            all().find(|candidate| {
                candidate
                    .attr("Identifier")
                    .is_some_and(|identifier| primary(identifier) == primary(language))
            })
        });
    let mut texts = Translations::new();
    let Some(chosen) = chosen else {
        return texts;
    };
    for element in chosen
        .children_named("TranslationUnit")
        .flat_map(|unit| unit.children_named("TranslationElement"))
    {
        let Some(id) = element.attr("RefId") else {
            continue;
        };
        for translation in element.children_named("Translation") {
            if let (Some(attribute), Some(text)) =
                (translation.attr("AttributeName"), translation.attr("Text"))
            {
                texts.insert(
                    (local_id(application, id).to_string(), attribute.to_string()),
                    text.to_string(),
                );
            }
        }
    }
    texts
}

fn parse_options(program: &Element<'_>, statics: &Element<'_>) -> Result<ProgramOptions, Error> {
    let Some(options) = statics.child("Options") else {
        return Ok(ProgramOptions::default());
    };
    let flag = |name: &'static str| {
        options
            .attr(name)
            .map(|text| parse_bool(text, name))
            .transpose()
            .map(|value| value.unwrap_or(false))
    };
    // The product's choice for invisible parameters only counts for products made for ETS 5
    // or later (from behaviour notes, unverified).
    let made_for_ets5 = program
        .attr("MinEtsVersion")
        .and_then(|version| version.split('.').next())
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 5);
    let invisible_parameters = match options.attr("DownloadInvisibleParameters") {
        Some("DefaultValue") if made_for_ets5 => InvisibleParameters::DefaultValue,
        Some("None") if made_for_ets5 => InvisibleParameters::None,
        Some("Background" | "DefaultValue" | "None") | None => InvisibleParameters::Background,
        Some(other) => return Err(invalid("DownloadInvisibleParameters", other)),
    };
    Ok(ProgramOptions {
        prefer_partial_download: flag("PreferPartialDownloadIfApplicationLoaded")?,
        no_memory_verify_mode: flag("LegacyNoMemoryVerifyMode")?,
        no_optimistic_write: flag("LegacyNoOptimisticWrite")?,
        ignore_property_write_errors: flag("LegacyDoNotReportPropertyWriteErrors")?,
        little_endian_parameters: options.attr("ParameterByteOrder") == Some("LittleEndian"),
        zero_terminate_text: flag("TextParameterZeroTerminate")?,
        invisible_parameters,
        skip_manufacturer_check: flag("LegacyDoNotCheckManufacturerId")?,
        ignore_second_application: flag("LegacyAllowPartialDownloadIfAp2Mismatch")?,
        prompt_before_full_download: flag("PromptBeforeFullDownload")?,
        patch_manufacturer_id: flag("LegacyPatchManufacturerIdInTaskSegment")?,
    })
}

/// Convert the `Dynamic` element. The XML reader bounds the nesting, so recursion is safe.
fn parse_dynamic(element: &Element<'_>, ids: &Ids<'_>) -> Result<Vec<DynamicNode>, Error> {
    let mut nodes = Vec::new();
    for child in &element.children {
        match child.name {
            "ParameterRefRef" => nodes.push(DynamicNode::ParameterRef(ids.of(child, "RefId")?)),
            "ComObjectRefRef" => nodes.push(DynamicNode::ComObjectRef(ids.of(child, "RefId")?)),
            "choose" => {
                let mut branches = Vec::new();
                for when in child.children_named("when") {
                    branches.push(When {
                        test: when.attr("test").map(str::to_string),
                        default: when
                            .attr("default")
                            .map(|text| parse_bool(text, "when default"))
                            .transpose()?
                            .unwrap_or(false),
                        children: parse_dynamic(when, ids)?,
                    });
                }
                nodes.push(DynamicNode::Choose {
                    parameter_ref: ids.of(child, "ParamRefId")?,
                    branches,
                });
            }
            "ParameterSeparator" => {
                nodes.push(DynamicNode::Separator(ids.text(child, "Text")));
            }
            "Channel" | "ChannelIndependentBlock" => nodes.push(DynamicNode::Channel {
                text: ids
                    .text_of(child, "Text")
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| ids.text(child, "Name")),
                children: parse_dynamic(child, ids)?,
            }),
            "ParameterBlock" => nodes.push(DynamicNode::Block {
                id: child
                    .attr("Id")
                    .map(|id| ids.local(id).to_string())
                    .unwrap_or_default(),
                text: ids
                    .text_of(child, "Text")
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| ids.text(child, "Name")),
                title: child.attr("ParamRefId").map(|id| ids.local(id).to_string()),
                access: Access::parse(child.attr("Access"))?.unwrap_or_default(),
                children: parse_dynamic(child, ids)?,
            }),
            _ => {
                let inner = parse_dynamic(child, ids)?;
                if !inner.is_empty() {
                    nodes.push(DynamicNode::Group(inner));
                }
            }
        }
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_sizes() {
        assert_eq!(parse_object_size("1 Bit").unwrap(), 1);
        assert_eq!(parse_object_size("4 Bit").unwrap(), 4);
        assert_eq!(parse_object_size("1 Byte").unwrap(), 8);
        assert_eq!(parse_object_size("14 Bytes").unwrap(), 112);
        assert!(parse_object_size("Bit").is_err());
        assert!(parse_object_size("3 Words").is_err());
        assert!(parse_object_size("9000 Bytes").is_err());
    }

    #[test]
    fn local_ids() {
        assert_eq!(local_id("M-1_A-2", "M-1_A-2_P-7_R-7"), "P-7_R-7");
        assert_eq!(local_id("M-1_A-2", "P-7_R-7"), "P-7_R-7");
        assert_eq!(local_id("M-1_A-2", "M-1_A-22_P-7"), "M-1_A-22_P-7");
    }

    #[test]
    fn missing_program_is_an_error() {
        assert!(matches!(
            ApplicationProgram::parse("<KNX/>"),
            Err(Error::Missing(_))
        ));
        assert!(ApplicationProgram::parse("<ApplicationProgram Id=\"x\"/>").is_err());
    }
}
