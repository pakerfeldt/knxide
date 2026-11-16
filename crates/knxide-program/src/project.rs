//! A whole project: every device with its product, its application program and whatever stands
//! in the way of downloading it.
//!
//! A project export (`.knxproj`) is a ZIP archive with one directory per project (`P-xxxx`,
//! holding `project.xml` and `0.xml`) and one per manufacturer (`M-xxxx`, holding
//! `Hardware.xml` and one XML file per application program). [`ProjectArchive`] reads such an
//! archive from memory; anything else that can hand out the same files by path (an unpacked
//! directory, for one) implements [`ProjectSource`].
//!
//! Only the XML files named above are ever read. Manufacturer baggage (plug-in installers,
//! libraries, help files, pictures) is not touched.
//!
//! The file layout is the one projects written by the tool that made the recordings have;
//! one real project has been read with it. Which attributes mark a KNX Secure
//! device is taken from published descriptions of the format and is unverified.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use knxide_core::{GroupAddress, IndividualAddress};

use crate::device::{DeviceData, GroupAddresses};
use crate::download::Download;
use crate::error::Error;
use crate::mask::MaskProfile;
use crate::plan::{Plan, PlanError, PlanOptions};
use crate::product::{ApplicationProgram, LoadProcedureStyle};
use crate::sim::SimulatedDevice;
use crate::xml::{self, Element};
use crate::zip::{Archive, ZipError};

/// Largest file read from a project, unpacked. Application programs of 15 MB occur.
pub const MAX_FILE_SIZE: usize = 128 * 1024 * 1024;

/// The access key of an installation without one (`BCUKey`).
const NO_KEY: u32 = 0xFFFF_FFFF;

/// Where the files of a project come from.
pub trait ProjectSource {
    /// Paths of the project's files, `/`-separated and relative, as in an archive. Paths that
    /// [`is_project_file`] rejects are ignored.
    fn paths(&self) -> Vec<String>;

    /// The content of the file at `path`.
    fn read(&self, path: &str) -> Result<Vec<u8>, Error>;
}

/// Whether `path` names a file a project is read from: `P-…/0.xml`, `P-…/project.xml` or an
/// XML file directly inside a manufacturer directory `M-…`. Everything else, and everything
/// under a `Baggages` directory in particular, is none of this crate's business.
pub fn is_project_file(path: &str) -> bool {
    let mut parts = path.split('/');
    let (Some(directory), Some(file), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if directory.starts_with("P-") {
        return file == "0.xml" || file == "project.xml";
    }
    directory.starts_with("M-")
        && file.len() > 4
        && file
            .get(file.len() - 4..)
            .is_some_and(|extension| extension.eq_ignore_ascii_case(".xml"))
}

/// A project archive (`.knxproj`) held in memory.
#[derive(Debug, Clone)]
pub struct ProjectArchive<'a> {
    archive: Archive<'a>,
}

fn archive_error(error: ZipError) -> Error {
    match error {
        ZipError::Encrypted => Error::PasswordProtected,
        other => Error::Archive(other.to_string()),
    }
}

impl<'a> ProjectArchive<'a> {
    /// Read the directory of the archive in `bytes`.
    ///
    /// A password-protected project is refused with [`Error::PasswordProtected`]: it shows
    /// either as encrypted entries or as an inner archive `P-….zip` in place of the project
    /// directory. No attempt is made to open it.
    pub fn open(bytes: &'a [u8]) -> Result<Self, Error> {
        let archive = Archive::open(bytes).map_err(archive_error)?;
        let entries = archive.entries();
        let inner_archive = entries.iter().any(|entry| {
            let name = entry.name.to_ascii_lowercase();
            name.starts_with("p-") && name.ends_with(".zip") && !name.contains('/')
        });
        let encrypted = entries
            .iter()
            .any(|entry| is_project_file(&entry.name) && entry.is_encrypted());
        if inner_archive || encrypted {
            return Err(Error::PasswordProtected);
        }
        Ok(Self { archive })
    }
}

impl ProjectSource for ProjectArchive<'_> {
    fn paths(&self) -> Vec<String> {
        self.archive
            .entries()
            .iter()
            .filter(|entry| is_project_file(&entry.name))
            .map(|entry| entry.name.clone())
            .collect()
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, Error> {
        if !is_project_file(path) {
            return Err(Error::Missing(format!("project file {path}")));
        }
        let entry = self
            .archive
            .entry(path)
            .ok_or_else(|| Error::Missing(format!("{path} in the project archive")))?;
        self.archive
            .read(entry, MAX_FILE_SIZE)
            .map_err(archive_error)
    }
}

/// What a project says about a product.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProductInfo {
    /// Order number.
    pub order_number: String,
    /// Product text.
    pub text: String,
}

/// A group address of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGroupAddress {
    /// Identifier, shortened to `GA-<n>`.
    pub id: String,
    /// The address.
    pub address: GroupAddress,
    /// Its name.
    pub name: String,
    /// The datapoint type the project gives it (`DatapointType`, for example `DPST-1-1`).
    pub datapoint_type: Option<String>,
}

/// A named range of group addresses: a main group or a middle group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGroupRange {
    /// First address of the range.
    pub start: u16,
    /// Last address of the range.
    pub end: u16,
    /// Its name.
    pub name: String,
    /// How deep it sits: 0 for a main group, 1 for a middle group.
    pub level: usize,
}

/// Why a device cannot be downloaded with this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obstacle {
    /// The device has no individual address in the project.
    NoAddress,
    /// The product has no application program.
    NoApplication,
    /// The application program's file is not in the project. The string is its path.
    MissingApplication(String),
    /// The application program cannot be read. The string says why.
    UnreadableApplication(String),
    /// The mask version is not one this crate has rules for.
    UnsupportedMask(u16),
    /// The load procedure comes from the mask, not from the product.
    ProcedureStyle,
    /// The product asks for a manufacturer plug-in to carry out its download. Alone, this
    /// leaves the device editable and comparable: see [`ProjectDevice::is_editable`].
    DownloadPlugin,
    /// A manufacturer plug-in replaces the parameter dialog and may decide which group objects
    /// are in use.
    UiPlugin,
    /// The device, or its application, uses KNX Secure.
    Secure,
    /// The installation has an access key. Devices would need it presented and written.
    AccessKey,
    /// The device has a second application program.
    SecondApplication,
    /// The download cannot be prepared from the product and project data. The string says why.
    Build(String),
    /// The load procedure contains a step this crate has no rules for. The string names it.
    UnsupportedStep(String),
    /// The plan does not go through against a model of a device that holds the application.
    /// The string says where it stops.
    Preview(String),
}

impl Obstacle {
    /// A short stable name for the kind of obstacle.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoAddress => "no-address",
            Self::NoApplication => "no-application",
            Self::MissingApplication(_) => "missing-application",
            Self::UnreadableApplication(_) => "unreadable-application",
            Self::UnsupportedMask(_) => "mask",
            Self::ProcedureStyle => "procedure-style",
            Self::DownloadPlugin => "download-plugin",
            Self::UiPlugin => "ui-plugin",
            Self::Secure => "secure",
            Self::AccessKey => "access-key",
            Self::SecondApplication => "second-application",
            Self::Build(_) => "build",
            Self::UnsupportedStep(_) => "unsupported-step",
            Self::Preview(_) => "preview",
        }
    }
}

impl fmt::Display for Obstacle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAddress => f.write_str("the device has no individual address"),
            Self::NoApplication => f.write_str("the product has no application program"),
            Self::MissingApplication(path) => {
                write!(f, "the application file {path} is not in the project")
            }
            Self::UnreadableApplication(why) => {
                write!(f, "the application program cannot be read: {why}")
            }
            Self::UnsupportedMask(mask) => write!(
                f,
                "mask version {mask:04X} is not supported (0021, 0701 and 0705 are)"
            ),
            Self::ProcedureStyle => f.write_str(
                "the load procedure comes from the mask, not from the product (default or \
                 merged style)",
            ),
            Self::DownloadPlugin => {
                f.write_str("the product needs a manufacturer plug-in for its download")
            }
            Self::UiPlugin => f.write_str(
                "plan may be wrong: object visibility is controlled by a manufacturer plugin",
            ),
            Self::Secure => f.write_str("the device uses KNX Secure"),
            Self::AccessKey => f.write_str("the project sets an access key for its devices"),
            Self::SecondApplication => f.write_str("the device has a second application program"),
            Self::Build(why) => write!(f, "the download cannot be prepared: {why}"),
            Self::UnsupportedStep(step) => write!(f, "unsupported step: {step}"),
            Self::Preview(why) => write!(f, "the plan stops against a modelled device: {why}"),
        }
    }
}

/// One device of a project.
#[derive(Debug, Clone)]
pub struct ProjectDevice {
    /// Identifier of the device in the project (`DeviceInstance/@Id`).
    pub id: String,
    /// Individual address; `None` for a device not placed on a line or without an address.
    pub address: Option<IndividualAddress>,
    /// The name the project gives the device; may be empty.
    pub name: String,
    /// Where the device is in the building, outermost first (building, floor, room), when the
    /// project places it; empty otherwise.
    pub location: Vec<String>,
    /// Order number and text of the product, when the project's hardware data has them.
    pub product: Option<ProductInfo>,
    /// Identifier of the application program, when the product has one.
    pub application_id: Option<String>,
    /// The application program, when its file could be read.
    pub application: Option<Arc<ApplicationProgram>>,
    /// What the project stores for the device.
    pub data: DeviceData,
    /// What stands in the way of a download; empty when there is nothing.
    pub obstacles: Vec<Obstacle>,
    /// The prepared download, when the data allows preparing one. Present even with
    /// obstacles, so that it can be looked at.
    pub download: Option<Download>,
    /// The plan against a model of a device that holds the application; see
    /// [`SimulatedDevice::holding`].
    pub preview: Option<Plan>,
}

impl ProjectDevice {
    /// Whether nothing stands in the way of a download.
    pub fn is_supported(&self) -> bool {
        self.obstacles.is_empty()
    }

    /// Whether the device can be edited and compared with what it holds, be it that it
    /// cannot be downloaded: a download could be prepared, and nothing stands in its way
    /// except, at most, that the product asks for a manufacturer plug-in to carry the download
    /// out ([`Obstacle::DownloadPlugin`]).
    ///
    /// Such a product has ordinary parameters, group objects and an ordinary load procedure;
    /// the plug-in concerns how the download is done, not how the device is configured. What
    /// it does differently on the bus is unknown, so the download stays with the tool the
    /// plug-in is made for until a comparison or a recording has shown that the ordinary
    /// procedure gives the same result. Comparing only reads.
    pub fn is_editable(&self) -> bool {
        self.download.is_some()
            && self
                .obstacles
                .iter()
                .all(|obstacle| *obstacle == Obstacle::DownloadPlugin)
    }
}

/// A space of the project's building structure: a building, a part of one, a room, a
/// distribution board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSpace {
    /// Its name and those of the spaces it sits in, outermost first.
    pub path: Vec<String>,
    /// What the project calls it: `Building`, `BuildingPart`, `Room`, `DistributionBoard`
    /// and so on; empty when the project does not say.
    pub kind: String,
}

/// A project with its devices.
#[derive(Debug, Clone)]
pub struct Project {
    /// Identifier, `P-xxxx`.
    pub id: String,
    /// Name; empty when the project file gives none.
    pub name: String,
    /// The group addresses, ascending.
    pub group_addresses: Vec<ProjectGroupAddress>,
    /// The named ranges the group addresses are sorted into, outermost first.
    pub group_ranges: Vec<ProjectGroupRange>,
    /// The group addresses by the identifiers devices link them with.
    pub addresses: GroupAddresses,
    /// The devices, in the order of the project file.
    pub devices: Vec<ProjectDevice>,
    /// The spaces of the building structure, in the order of the project file.
    pub spaces: Vec<ProjectSpace>,
}

/// What `Hardware.xml` of one manufacturer says.
#[derive(Debug, Default)]
struct Hardware {
    products: HashMap<String, ProductInfo>,
    /// Application program identifiers per hardware-to-program entry, and whether the hardware
    /// entry says the product cannot be downloaded without its plug-in.
    programs: HashMap<String, (Vec<String>, bool)>,
}

fn flag(text: Option<&str>) -> bool {
    matches!(text.map(str::trim), Some("1" | "true"))
}

/// The `M-xxxx` in front of an identifier.
fn manufacturer_of(id: &str) -> Option<&str> {
    let end = id.find('_').unwrap_or(id.len());
    id.get(..end).filter(|part| part.starts_with("M-"))
}

fn text_of(source: &dyn ProjectSource, path: &str) -> Result<String, Error> {
    String::from_utf8(source.read(path)?).map_err(|_| Error::Invalid {
        what: "text encoding",
        value: path.to_string(),
    })
}

fn parse_hardware(text: &str) -> Result<Hardware, Error> {
    let root = xml::parse(text)?;
    let mut hardware = Hardware::default();
    let mut pending = vec![&root];
    while let Some(element) = pending.pop() {
        if element.name == "Hardware" && element.attr("Id").is_some() {
            let needs_plugin = flag(element.attr("NoDownloadWithoutPlugin"));
            for product in element
                .child("Products")
                .into_iter()
                .flat_map(|list| list.children_named("Product"))
            {
                if let Some(id) = product.attr("Id") {
                    hardware.products.insert(
                        id.to_string(),
                        ProductInfo {
                            order_number: product.attr("OrderNumber").unwrap_or_default().into(),
                            text: product.attr("Text").unwrap_or_default().into(),
                        },
                    );
                }
            }
            for program in element
                .child("Hardware2Programs")
                .into_iter()
                .flat_map(|list| list.children_named("Hardware2Program"))
            {
                if let Some(id) = program.attr("Id") {
                    let applications = program
                        .children_named("ApplicationProgramRef")
                        .filter_map(|reference| reference.attr("RefId"))
                        .map(str::to_string)
                        .collect();
                    hardware
                        .programs
                        .insert(id.to_string(), (applications, needs_plugin));
                }
            }
        } else {
            pending.extend(element.children.iter());
        }
    }
    Ok(hardware)
}

/// A device element with the area and line it sits on.
struct Placed<'e, 'a> {
    element: &'e Element<'a>,
    line: Option<(u8, u8)>,
}

/// Every `DeviceInstance` under `element`, with area and line addresses picked up on the way.
fn collect_devices<'e, 'a>(
    element: &'e Element<'a>,
    area: Option<u8>,
    line: Option<(u8, u8)>,
    out: &mut Vec<Placed<'e, 'a>>,
) {
    for child in &element.children {
        let number = || {
            child
                .attr("Address")
                .and_then(|text| text.trim().parse().ok())
        };
        match child.name {
            "DeviceInstance" => out.push(Placed {
                element: child,
                line,
            }),
            "Area" => collect_devices(child, number(), None, out),
            "Line" => {
                let line = area.zip(number());
                collect_devices(child, area, line, out);
            }
            _ => collect_devices(child, area, line, out),
        }
    }
}

/// Whether the project holds key material for the device, or links it to a secured group
/// address. Unverified: taken from published descriptions of the project format.
fn is_secure(device: &Element<'_>) -> bool {
    const KEYS: [&str; 6] = [
        "ToolKey",
        "LoadedToolKey",
        "DeviceAuthenticationCode",
        "LoadedDeviceAuthenticationCode",
        "DeviceManagementPassword",
        "LoadedDeviceManagementPassword",
    ];
    device.children_named("Security").any(|security| {
        KEYS.iter()
            .any(|key| security.attr(key).is_some_and(|value| !value.is_empty()))
    })
}

impl Project {
    /// Read the first project of `source`: its devices, their products and application
    /// programs, and for each device whether it can be downloaded.
    ///
    /// Fails only when the project itself cannot be read. A product or application that is
    /// missing or unreadable becomes an [`Obstacle`] of the devices that use it.
    pub fn load(source: &dyn ProjectSource) -> Result<Self, Error> {
        Self::load_in(source, None)
    }

    /// Like [`Project::load`], with the products' texts in `language` where they have them;
    /// see [`ApplicationProgram::parse_in`].
    pub fn load_in(source: &dyn ProjectSource, language: Option<&str>) -> Result<Self, Error> {
        let paths = source.paths();
        let installation = paths
            .iter()
            .filter(|path| is_project_file(path))
            .find(|path| path.starts_with("P-") && path.ends_with("/0.xml"))
            .ok_or_else(|| Error::Missing("P-…/0.xml in the project".into()))?;
        let id = installation
            .split('/')
            .next()
            .unwrap_or_default()
            .to_string();

        let project_file = format!("{id}/project.xml");
        let name = if paths.contains(&project_file) {
            let text = text_of(source, &project_file)?;
            xml::parse(&text)?
                .find("ProjectInformation")
                .and_then(|information| information.attr("Name"))
                .unwrap_or_default()
                .to_string()
        } else {
            String::new()
        };

        let text = text_of(source, installation)?;
        let root = xml::parse(&text)?;
        let addresses = GroupAddresses::parse_project(&text)?;
        let mut group_addresses = Vec::new();
        let mut group_ranges = Vec::new();
        let mut secured = Vec::new();
        collect_addresses(
            &root,
            0,
            &addresses,
            &mut group_addresses,
            &mut group_ranges,
            &mut secured,
        );
        group_addresses.sort_by_key(|entry| entry.address);
        let mut locations = HashMap::new();
        let mut spaces = Vec::new();
        if let Some(structure) = root.find("Locations") {
            collect_locations(structure, &mut Vec::new(), &mut locations, &mut spaces);
        }

        let access_key = root
            .find("Installation")
            .and_then(|installation| installation.attr("BCUKey"))
            .and_then(|key| key.trim().parse::<u32>().ok())
            .is_some_and(|key| key != NO_KEY);

        let mut placed = Vec::new();
        collect_devices(&root, None, None, &mut placed);

        let mut loader = Loader {
            source,
            paths: &paths,
            language,
            hardware: HashMap::new(),
            applications: HashMap::new(),
        };
        let mut devices = Vec::with_capacity(placed.len());
        for device in &placed {
            let mut device = loader.device(device, &addresses, &secured, access_key);
            device.location = locations.remove(&device.id).unwrap_or_default();
            devices.push(device);
        }
        Ok(Self {
            id,
            name,
            group_addresses,
            group_ranges,
            addresses,
            devices,
            spaces,
        })
    }

    /// The device with identifier `id`.
    pub fn device(&self, id: &str) -> Option<&ProjectDevice> {
        self.devices.iter().find(|device| device.id == id)
    }

    /// The group address with the 16-bit value `address`.
    pub fn group_address(&self, address: GroupAddress) -> Option<&ProjectGroupAddress> {
        self.group_addresses
            .binary_search_by_key(&address, |entry| entry.address)
            .ok()
            .and_then(|at| self.group_addresses.get(at))
    }
}

/// Collect group addresses and the named ranges around them. The XML reader bounds the
/// nesting, so recursion is safe.
fn collect_addresses(
    element: &Element<'_>,
    level: usize,
    addresses: &GroupAddresses,
    out: &mut Vec<ProjectGroupAddress>,
    ranges: &mut Vec<ProjectGroupRange>,
    secured: &mut Vec<String>,
) {
    for child in &element.children {
        match child.name {
            "GroupAddress" => {
                let Some(id) = child.attr("Id") else {
                    continue;
                };
                let Some(address) = addresses.get(id) else {
                    continue;
                };
                let short = id.rfind("GA-").and_then(|at| id.get(at..)).unwrap_or(id);
                if child.attr("Security").is_some_and(|value| value == "On") {
                    secured.push(short.to_string());
                }
                out.push(ProjectGroupAddress {
                    id: short.to_string(),
                    address,
                    name: child.attr("Name").unwrap_or_default().to_string(),
                    datapoint_type: child
                        .attr("DatapointType")
                        .filter(|text| !text.is_empty())
                        .map(str::to_string),
                });
            }
            "GroupRange" => {
                let bound = |name: &str| {
                    child
                        .attr(name)
                        .and_then(|text| text.trim().parse::<u16>().ok())
                };
                if let (Some(start), Some(end)) = (bound("RangeStart"), bound("RangeEnd")) {
                    ranges.push(ProjectGroupRange {
                        start,
                        end,
                        name: child.attr("Name").unwrap_or_default().to_string(),
                        level,
                    });
                }
                collect_addresses(child, level + 1, addresses, out, ranges, secured);
            }
            _ => collect_addresses(child, level, addresses, out, ranges, secured),
        }
    }
}

/// Collect, per device identifier, the names of the spaces it sits in, outermost first, and
/// every space with its kind.
fn collect_locations(
    element: &Element<'_>,
    path: &mut Vec<String>,
    out: &mut HashMap<String, Vec<String>>,
    spaces: &mut Vec<ProjectSpace>,
) {
    for child in &element.children {
        match child.name {
            "Space" => {
                path.push(child.attr("Name").unwrap_or_default().to_string());
                spaces.push(ProjectSpace {
                    path: path.clone(),
                    kind: child.attr("Type").unwrap_or_default().to_string(),
                });
                collect_locations(child, path, out, spaces);
                path.pop();
            }
            "DeviceInstanceRef" => {
                if let Some(id) = child.attr("RefId") {
                    out.entry(id.to_string()).or_insert_with(|| path.clone());
                }
            }
            _ => {}
        }
    }
}

/// Reads hardware and application files once each.
struct Loader<'s> {
    source: &'s dyn ProjectSource,
    paths: &'s [String],
    language: Option<&'s str>,
    hardware: HashMap<String, Option<Arc<Hardware>>>,
    applications: HashMap<String, Result<Arc<ApplicationProgram>, Obstacle>>,
}

impl Loader<'_> {
    fn hardware(&mut self, manufacturer: &str) -> Option<Arc<Hardware>> {
        if let Some(known) = self.hardware.get(manufacturer) {
            return known.clone();
        }
        let path = format!("{manufacturer}/Hardware.xml");
        let loaded = self
            .paths
            .contains(&path)
            .then(|| text_of(self.source, &path).and_then(|text| parse_hardware(&text)))
            .and_then(Result::ok)
            .map(Arc::new);
        self.hardware
            .insert(manufacturer.to_string(), loaded.clone());
        loaded
    }

    fn application(&mut self, id: &str) -> Result<Arc<ApplicationProgram>, Obstacle> {
        if let Some(known) = self.applications.get(id) {
            return known.clone();
        }
        let path = format!("{}/{id}.xml", manufacturer_of(id).unwrap_or_default());
        let loaded = if self.paths.contains(&path) {
            text_of(self.source, &path)
                .and_then(|text| ApplicationProgram::parse_in(&text, self.language))
                .map(Arc::new)
                .map_err(|error| match error {
                    Error::UnsupportedMask(mask) => Obstacle::UnsupportedMask(mask),
                    other => Obstacle::UnreadableApplication(other.to_string()),
                })
        } else {
            Err(Obstacle::MissingApplication(path))
        };
        self.applications.insert(id.to_string(), loaded.clone());
        loaded
    }

    fn device(
        &mut self,
        placed: &Placed<'_, '_>,
        addresses: &GroupAddresses,
        secured: &[String],
        access_key: bool,
    ) -> ProjectDevice {
        let element = placed.element;
        let mut obstacles = Vec::new();
        // What the project stores for the device may be beyond this crate; the other devices
        // are still worth listing.
        let (data, readable) = match DeviceData::from_element(element) {
            Ok(data) => (data, true),
            Err(error) => {
                obstacles.push(Obstacle::Build(error.to_string()));
                let address = element
                    .attr("Address")
                    .and_then(|text| text.trim().parse().ok());
                let data = DeviceData {
                    address,
                    ..DeviceData::default()
                };
                (data, false)
            }
        };
        let address = placed
            .line
            .zip(data.address)
            .and_then(|((area, line), device)| {
                IndividualAddress::new(area, line, u8::try_from(device).ok()?)
            });
        if address.is_none() {
            obstacles.push(Obstacle::NoAddress);
        }

        let product_id = element.attr("ProductRefId").unwrap_or_default();
        let program_id = element.attr("Hardware2ProgramRefId").unwrap_or_default();
        let hardware = manufacturer_of(product_id)
            .or_else(|| manufacturer_of(program_id))
            .and_then(|manufacturer| self.hardware(manufacturer));
        let product = hardware
            .as_ref()
            .and_then(|hardware| hardware.products.get(product_id).cloned());
        let (application_ids, needs_plugin) = hardware
            .as_ref()
            .and_then(|hardware| hardware.programs.get(program_id).cloned())
            .unwrap_or_default();
        if application_ids.len() > 1 {
            obstacles.push(Obstacle::SecondApplication);
        }
        let application_id = application_ids.into_iter().next();
        let application = match &application_id {
            Some(id) => match self.application(id) {
                Ok(application) => Some(application),
                Err(obstacle) => {
                    obstacles.push(obstacle);
                    None
                }
            },
            None => {
                obstacles.push(Obstacle::NoApplication);
                None
            }
        };

        let links_secured = data
            .object_links
            .iter()
            .flat_map(|entry| &entry.links)
            .any(|link| secured.iter().any(|id| link.ends_with(id.as_str())));
        if is_secure(element) || links_secured {
            obstacles.push(Obstacle::Secure);
        }
        if access_key {
            obstacles.push(Obstacle::AccessKey);
        }

        let mut download = None;
        let mut preview = None;
        if let Some(program) = &application {
            if MaskProfile::for_mask(program.mask_version).is_err() {
                obstacles.push(Obstacle::UnsupportedMask(program.mask_version));
            }
            if program.load_procedure_style != LoadProcedureStyle::Product {
                obstacles.push(Obstacle::ProcedureStyle);
            }
            if program.download_plugin.is_some() || needs_plugin {
                obstacles.push(Obstacle::DownloadPlugin);
            }
            if program.ui_plugin.is_some() {
                obstacles.push(Obstacle::UiPlugin);
            }
            if program.secure {
                obstacles.push(Obstacle::Secure);
            }
            let buildable = readable
                && !obstacles.iter().any(|obstacle| {
                    matches!(
                        obstacle,
                        Obstacle::UnsupportedMask(_) | Obstacle::ProcedureStyle
                    )
                });
            if buildable {
                match Download::prepare(program, &data, addresses) {
                    Ok(prepared) => {
                        let mut model = SimulatedDevice::holding(&prepared);
                        match prepared.plan(&mut model, &PlanOptions::default()) {
                            Ok(plan) => preview = Some(plan),
                            Err(failure) => obstacles.push(match failure.error {
                                PlanError::Unsupported(what) => Obstacle::UnsupportedStep(what),
                                other => Obstacle::Preview(other.to_string()),
                            }),
                        }
                        download = Some(prepared);
                    }
                    Err(error) => obstacles.push(Obstacle::Build(error.to_string())),
                }
            }
        }
        obstacles.dedup();

        ProjectDevice {
            id: element.attr("Id").unwrap_or_default().to_string(),
            address,
            name: element.attr("Name").unwrap_or_default().to_string(),
            location: Vec::new(),
            product,
            application_id,
            application,
            data,
            obstacles,
            download,
            preview,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::Builder;

    #[test]
    fn which_files_belong_to_a_project() {
        for path in [
            "P-0001/0.xml",
            "P-0001/project.xml",
            "M-00FA/Hardware.xml",
            "M-00FA/M-00FA_A-0001-01-0000.xml",
            "M-00FA/Catalog.XML",
        ] {
            assert!(is_project_file(path), "{path}");
        }
        for path in [
            "P-0001/1.xml",
            "P-0001/BinaryData/x.xml",
            "M-00FA/Baggages/tool.dll",
            "M-00FA/Baggages/notes.xml",
            "M-00FA/setup.msi",
            "M-00FA/.xml",
            "knx_master.xml",
            "P-0001.zip",
            "X-1/0.xml",
            "",
        ] {
            assert!(!is_project_file(path), "{path}");
        }
    }

    #[test]
    fn archive_hands_out_project_files_only() {
        let bytes = Builder::new()
            .stored("P-0001/project.xml", b"<p/>")
            .deflated("P-0001/0.xml", b"<KNX/>")
            .stored("M-00FA/Baggages/tool.dll", b"MZ")
            .stored("M-00FA/Hardware.xml", b"<h/>")
            .finish();
        let archive = ProjectArchive::open(&bytes).unwrap();
        assert_eq!(
            archive.paths(),
            ["P-0001/project.xml", "P-0001/0.xml", "M-00FA/Hardware.xml"]
        );
        assert_eq!(archive.read("P-0001/0.xml").unwrap(), b"<KNX/>");
        assert!(matches!(
            archive.read("M-00FA/Baggages/tool.dll"),
            Err(Error::Missing(_))
        ));
        assert!(matches!(
            archive.read("M-00FA/Other.xml"),
            Err(Error::Missing(_))
        ));
    }

    #[test]
    fn protected_projects_are_refused() {
        // An inner archive in place of the project directory.
        let nested = Builder::new()
            .stored("P-0001.zip", b"PK...")
            .stored("M-00FA/Hardware.xml", b"<h/>")
            .finish();
        assert_eq!(
            ProjectArchive::open(&nested).unwrap_err(),
            Error::PasswordProtected
        );
        // Encrypted entries.
        let encrypted = Builder::new()
            .raw("P-0001/0.xml", 99, 1, b"????", 4, 0)
            .finish();
        assert_eq!(
            ProjectArchive::open(&encrypted).unwrap_err(),
            Error::PasswordProtected
        );
        // Encrypted baggage alone does not matter: it is never read.
        let baggage = Builder::new()
            .stored("P-0001/0.xml", b"<KNX/>")
            .raw("M-00FA/Baggages/x.bin", 99, 1, b"????", 4, 0)
            .finish();
        assert!(ProjectArchive::open(&baggage).is_ok());
        assert!(matches!(
            ProjectArchive::open(b"not an archive"),
            Err(Error::Archive(_))
        ));
    }

    #[test]
    fn manufacturer_prefixes() {
        assert_eq!(manufacturer_of("M-00FA_A-0001-01-0000"), Some("M-00FA"));
        assert_eq!(manufacturer_of("M-00FA_H-1_P-2"), Some("M-00FA"));
        assert_eq!(manufacturer_of("P-0001-0_DI-1"), None);
        assert_eq!(manufacturer_of(""), None);
    }
}
