//! Reading a whole project: the made-up one of the test kit, from an archive and from loose
//! files.

use std::collections::HashMap;

use knxide_program::project::is_project_file;
use knxide_program::{Error, Obstacle, Project, ProjectArchive, ProjectSource};
use knxide_testkit::project::{DEVICES, PROJECT_ID, PROJECT_NAME, archive, files};
use knxide_testkit::zip;

/// Loose files, as an unpacked project directory would hand them out. Counts what is read.
struct Files {
    files: HashMap<String, Vec<u8>>,
    read: std::cell::RefCell<Vec<String>>,
}

impl Files {
    fn new(files: Vec<(String, Vec<u8>)>) -> Self {
        Self {
            files: files.into_iter().collect(),
            read: Default::default(),
        }
    }
}

impl ProjectSource for Files {
    fn paths(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, Error> {
        self.read.borrow_mut().push(path.to_string());
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| Error::Missing(path.to_string()))
    }
}

fn obstacles(project: &Project, id: &str) -> Vec<&'static str> {
    project
        .device(id)
        .unwrap()
        .obstacles
        .iter()
        .map(Obstacle::code)
        .collect()
}

#[test]
fn inventory_of_the_made_up_project() {
    let bytes = archive();
    let source = ProjectArchive::open(&bytes).unwrap();
    let project = Project::load(&source).unwrap();
    assert_eq!(project.id, PROJECT_ID);
    assert_eq!(project.name, PROJECT_NAME);
    assert_eq!(project.devices.len(), DEVICES.len());
    for (device, (id, address, supported)) in project.devices.iter().zip(DEVICES) {
        assert_eq!(device.id, id);
        assert_eq!(
            device.address.map(|address| address.to_string()),
            address.map(str::to_string)
        );
        assert_eq!(
            device.is_supported(),
            supported,
            "{id}: {:?}",
            device.obstacles
        );
        // A supported device always has a plan to show.
        assert!(device.preview.is_some() || !supported, "{id}");
    }

    let dimmer = &project.devices[0];
    assert_eq!(dimmer.name, "Hall dimmer");
    let product = dimmer.product.as_ref().unwrap();
    assert_eq!(
        (product.order_number.as_str(), product.text.as_str()),
        ("D-100", "Demo dimmer")
    );
    let application = dimmer.application.as_ref().unwrap();
    assert_eq!(application.name, "Demo dimming 0102/0.3");
    assert_eq!(application.mask_version, 0x0021);
    let download = dimmer.download.as_ref().unwrap();
    assert!(download.kind.full && download.fallback.is_none());
    assert_eq!(download.tables.as_ref().unwrap().addresses.len(), 2);

    // The sensor's product prefers a partial download; the full one stands by.
    let sensor = project.device("P-0001-0_DI-3").unwrap();
    let download = sensor.download.as_ref().unwrap();
    assert!(!download.kind.full && download.fallback.is_some());
    assert!(!sensor.preview.as_ref().unwrap().kind.full);

    assert_eq!(obstacles(&project, "P-0001-0_DI-4"), ["ui-plugin"]);
    assert_eq!(
        project.device("P-0001-0_DI-4").unwrap().obstacles[0].to_string(),
        "plan may be wrong: object visibility is controlled by a manufacturer plugin"
    );
    // The plan of a device with a dialog plug-in is still there to look at.
    assert!(project.device("P-0001-0_DI-4").unwrap().download.is_some());
    assert_eq!(obstacles(&project, "P-0001-0_DI-5"), ["download-plugin"]);
    // Which devices can be edited and compared: those that can be downloaded, and the one
    // whose product asks for a download plug-in and nothing else. A dialog plug-in, another
    // mask, a missing application or address leave nothing to edit.
    let editable: Vec<&str> = project
        .devices
        .iter()
        .filter(|device| device.is_editable())
        .map(|device| device.id.as_str())
        .collect();
    assert_eq!(
        editable,
        [
            "P-0001-0_DI-1",
            "P-0001-0_DI-2",
            "P-0001-0_DI-3",
            "P-0001-0_DI-5"
        ]
    );
    let actuator = project.device("P-0001-0_DI-5").unwrap();
    assert!(!actuator.is_supported() && actuator.download.is_some());
    assert!(
        project
            .devices
            .iter()
            .all(|device| !device.is_supported() || device.is_editable())
    );
    assert_eq!(
        obstacles(&project, "P-0001-0_DI-6"),
        ["mask", "procedure-style"]
    );
    assert_eq!(
        obstacles(&project, "P-0001-0_DI-7"),
        ["missing-application"]
    );
    assert_eq!(
        obstacles(&project, "P-0001-0_DI-8"),
        ["no-address", "no-application"]
    );

    assert_eq!(project.group_addresses.len(), 8);
    assert_eq!(project.group_addresses[0].id, "GA-5");
    assert_eq!(
        project.group_addresses[0].datapoint_type.as_deref(),
        Some("DPST-1-1")
    );
    assert_eq!(project.group_addresses[7].datapoint_type, None);
    // Main and middle groups, outermost first.
    let ranges: Vec<(&str, usize)> = project
        .group_ranges
        .iter()
        .map(|range| (range.name.as_str(), range.level))
        .collect();
    assert_eq!(
        ranges,
        [
            ("Lighting", 0),
            ("Scenes", 1),
            ("Switching", 1),
            ("Status", 1),
            ("Heating", 0),
            ("Valves", 1)
        ]
    );
    // Where the devices are.
    assert_eq!(
        project.devices[0].location,
        ["Demo house", "Ground floor", "Hall"]
    );
    assert_eq!(project.devices[5].location, ["Demo house", "Cabinet"]);
    assert!(project.devices[6].location.is_empty());
    let valve = project
        .group_address(knxide_core::GroupAddress::from_raw(4660))
        .unwrap();
    assert_eq!(valve.name, "Kitchen valve");
}

#[test]
fn only_project_files_are_read() {
    let source = Files::new(files());
    let project = Project::load(&source).unwrap();
    assert_eq!(project.devices.len(), DEVICES.len());
    let read = source.read.borrow();
    assert!(read.iter().all(|path| is_project_file(path)), "{read:?}");
    assert!(!read.iter().any(|path| path.contains("Baggages")));
    // Each file once, however many devices share it.
    let mut unique = read.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), read.len());

    // The archive does not hand out baggage even when asked.
    let bytes = archive();
    let archive = ProjectArchive::open(&bytes).unwrap();
    assert!(!archive.paths().iter().any(|path| path.contains("Baggages")));
    assert!(archive.read("M-00FA/Baggages/plugin.dll").is_err());
}

#[test]
fn projects_that_cannot_be_read() {
    // No project directory.
    let empty = zip::stored(&[("M-00FA/Hardware.xml".to_string(), b"<KNX/>".to_vec())]);
    let source = ProjectArchive::open(&empty).unwrap();
    assert!(matches!(Project::load(&source), Err(Error::Missing(_))));
    // A project file that is not XML.
    let broken = zip::stored(&[("P-0001/0.xml".to_string(), b"<KNX><Project>".to_vec())]);
    let source = ProjectArchive::open(&broken).unwrap();
    assert!(matches!(Project::load(&source), Err(Error::Xml(_))));
    // A protected project: the project directory is an inner archive.
    let protected = zip::stored(&[("P-0001.zip".to_string(), b"PK".to_vec())]);
    assert_eq!(
        ProjectArchive::open(&protected).unwrap_err(),
        Error::PasswordProtected
    );
    assert!(
        Error::PasswordProtected
            .to_string()
            .contains("password-protected")
    );
}

#[test]
fn installation_wide_obstacles() {
    let with = |from: &str, to: &str| {
        let files = files()
            .into_iter()
            .map(|(path, content)| {
                if path == "P-0001/0.xml" {
                    let text = String::from_utf8(content).unwrap();
                    assert!(text.contains(from), "{from}");
                    (path, text.replace(from, to).into_bytes())
                } else {
                    (path, content)
                }
            })
            .collect();
        Project::load(&Files::new(files)).unwrap()
    };
    // An access key for the installation stands in the way of every device.
    let keyed = with(r#"BCUKey="4294967295""#, r#"BCUKey="305419896""#);
    assert!(keyed.devices.iter().all(|device| !device.is_supported()));
    assert_eq!(obstacles(&keyed, "P-0001-0_DI-1"), ["access-key"]);
    // Key material for one device.
    let secure = with(
        r#"<ParameterInstanceRef RefId="M-00FA_A-0102-03-0000_P-9_R-9" Value="2" />"#,
        r#"<ParameterInstanceRef RefId="M-00FA_A-0102-03-0000_P-9_R-9" Value="2" />
           </ParameterInstanceRefs><Security ToolKey="AAAA" /><ParameterInstanceRefs>"#,
    );
    assert_eq!(obstacles(&secure, "P-0001-0_DI-1"), ["secure"]);
    assert!(secure.device("P-0001-0_DI-2").unwrap().is_supported());
    // A linked group address that is secured.
    let secured = with(
        r#"Address="4660" Name="Kitchen valve""#,
        r#"Address="4660" Name="Kitchen valve" Security="On""#,
    );
    assert_eq!(obstacles(&secured, "P-0001-0_DI-2"), ["secure"]);
    // A device outside any line has no address.
    let unplaced = with(
        r#"<Topology>"#,
        r#"<Topology><UnassignedDevices><DeviceInstance Id="P-0001-0_DI-9" Address="5" /></UnassignedDevices>"#,
    );
    assert!(obstacles(&unplaced, "P-0001-0_DI-9").contains(&"no-address"));
}

#[test]
fn a_device_the_project_data_of_which_cannot_be_read_does_not_hide_the_others() {
    let files = files()
        .into_iter()
        .map(|(path, content)| {
            if path == "P-0001/0.xml" {
                let text = String::from_utf8(content)
                    .unwrap()
                    .replace(r#"CheckSums=""#, r#"CheckSums="!!!"#);
                (path, text.into_bytes())
            } else {
                (path, content)
            }
        })
        .collect();
    let project = Project::load(&Files::new(files)).unwrap();
    assert_eq!(project.devices.len(), DEVICES.len());
    let sensor = project.device("P-0001-0_DI-3").unwrap();
    assert_eq!(obstacles(&project, "P-0001-0_DI-3"), ["build"]);
    assert!(sensor.download.is_none());
    assert_eq!(sensor.address.unwrap().to_string(), "1.1.110");
    assert!(project.device("P-0001-0_DI-1").unwrap().is_supported());
}

#[test]
fn a_second_application_program_is_an_obstacle() {
    let files = files()
        .into_iter()
        .map(|(path, content)| {
            if path == "M-00FA/Hardware.xml" {
                let text = String::from_utf8(content).unwrap().replace(
                    r#"<ApplicationProgramRef RefId="M-00FA_A-0201-01-0000" />"#,
                    r#"<ApplicationProgramRef RefId="M-00FA_A-0201-01-0000" /><ApplicationProgramRef RefId="M-00FA_A-0202-01-0000" />"#,
                );
                (path, text.into_bytes())
            } else {
                (path, content)
            }
        })
        .collect();
    let project = Project::load(&Files::new(files)).unwrap();
    assert_eq!(obstacles(&project, "P-0001-0_DI-2"), ["second-application"]);
}
