//! Editing a device in code: validation, the view a person sees, visibility after an edit,
//! what an edit does to the download, and reading a comparison back into parameters and
//! links. The products are the made-up ones of the test kit; one test starts from the recorded
//! devices when `KNXIDE_PROGRAM_FIXTURES` is set.

use std::path::PathBuf;

use knxide_core::GroupAddress;
use knxide_program::compare::{Difference, Finding, decode, table_gaps};
use knxide_program::edit::{EditError, changes, check_value};
use knxide_program::product::{ParameterKind, ParameterLocation};
use knxide_program::view::{Control, DeviceView, Item, ParameterView, Standing};
use knxide_program::{
    ApplicationProgram, Configuration, DeviceData, Download, Edits, GroupAddresses, Project,
    ProjectArchive, view,
};
use knxide_testkit::project::{
    BCU2_ADDRESSES, BCU2_DEVICE, BCU2_PRODUCT, M112_DEVICE, M112_PRODUCT, archive,
};

fn ga(raw: u16) -> GroupAddress {
    GroupAddress::from_raw(raw)
}

struct Case {
    program: ApplicationProgram,
    baseline: DeviceData,
    addresses: GroupAddresses,
}

fn bcu2() -> Case {
    Case {
        program: ApplicationProgram::parse(BCU2_PRODUCT).unwrap(),
        baseline: DeviceData::parse(BCU2_DEVICE).unwrap(),
        addresses: GroupAddresses::parse_list(BCU2_ADDRESSES).unwrap(),
    }
}

fn m112() -> Case {
    let mut addresses = GroupAddresses::new();
    addresses.insert("GA-7", ga(4660));
    Case {
        program: ApplicationProgram::parse(M112_PRODUCT).unwrap(),
        baseline: DeviceData::parse(M112_DEVICE).unwrap(),
        addresses,
    }
}

impl Case {
    fn view(&self, edits: &Edits) -> DeviceView {
        let (effective, addresses) = edits.apply(&self.program, &self.baseline, &self.addresses);
        view(&self.program, &self.baseline, &effective, &addresses).unwrap()
    }

    fn download(&self, edits: &Edits) -> Download {
        let (effective, addresses) = edits.apply(&self.program, &self.baseline, &self.addresses);
        Download::prepare(&self.program, &effective, &addresses).unwrap()
    }
}

fn parameters(view: &DeviceView) -> Vec<&ParameterView> {
    view.pages
        .iter()
        .flat_map(|page| &page.items)
        .filter_map(|item| match item {
            Item::Parameter(parameter) => Some(parameter),
            Item::Text(_) => None,
        })
        .collect()
}

fn labels(view: &DeviceView) -> Vec<&str> {
    parameters(view)
        .iter()
        .map(|parameter| parameter.label.as_str())
        .collect()
}

#[test]
fn the_view_of_a_device() {
    let case = bcu2();
    let view = case.view(&Edits::default());
    // One page, in the channel and with the title the product gives.
    assert_eq!(view.pages.len(), 1);
    let page = &view.pages[0];
    assert_eq!(
        (page.channel.as_str(), page.title.as_str()),
        ("Main", "General")
    );
    assert_eq!(page.id, "PB-1");
    // Mode 2 (the project's value): the high nibble is in use, the low one and the spare are
    // not. The hidden parameter never shows; the one without a value has no text and is left
    // out; the separator is a line of text.
    assert_eq!(
        labels(&view),
        ["Level", "Mode > all", "Hardware revision", "High"]
    );
    assert!(
        page.items
            .contains(&Item::Text("What follows depends on the mode.".into()))
    );

    let [level, mode, hardware, high] = parameters(&view)[..] else {
        panic!("four parameters")
    };
    // A number with the type's limits and a unit; the product's reference default is 6.
    assert_eq!(
        level.control,
        Control::Number {
            minimum: 0,
            maximum: 255
        }
    );
    assert_eq!((level.value.as_str(), level.suffix.as_str()), ("6", "%"));
    assert_eq!(level.standing, Standing::Default);
    // An enumeration in the product's order; the project chose "Dimming".
    assert_eq!(
        mode.control,
        Control::Choice(vec![
            ("0".into(), "Off".into()),
            ("1".into(), "Switching".into()),
            ("2".into(), "Dimming".into()),
            ("3".into(), "Dimming with scenes".into()),
        ])
    );
    assert_eq!(
        (mode.value.as_str(), mode.value_text.as_str()),
        ("2", "Dimming")
    );
    assert_eq!(mode.default_text, "Switching");
    assert_eq!(mode.standing, Standing::Project);
    // Read-only: shown, without a control.
    assert_eq!(hardware.control, Control::Fixed);
    assert_eq!(hardware.value, "7");
    assert_eq!(high.value_text, "nine");

    // The group objects in use, with what the product says about them and the project's
    // links.
    let objects: Vec<(u16, &str, &str, Vec<u16>)> = view
        .objects
        .iter()
        .map(|object| {
            (
                object.number,
                object.function.as_str(),
                object.datapoint_type.as_str(),
                object.links.iter().map(|link| link.raw()).collect(),
            )
        })
        .collect();
    assert_eq!(
        objects,
        [
            (0, "Switch", "DPST-1-1", vec![2562]),
            (1, "Brightness value", "DPST-5-1", vec![2305]),
        ]
    );
    assert_eq!(view.objects[0].reference, "O-0_R-1");
    assert_eq!(view.objects[0].name, "Channel A");
    assert_eq!(view.objects[0].attributes.size_in_bit, Some(1));
    assert!(view.unused.is_empty());
}

/// Products made for older tools keep parameters a person may change in a block that is not
/// shown and leave the page they belong on empty; the display order ties them together.
#[test]
fn parameters_in_a_block_that_is_not_shown_go_by_display_order() {
    let id = "M-00FA_A-0102-03-0000";
    let dynamic_from = BCU2_PRODUCT.find("<Dynamic>").unwrap();
    let dynamic_to = BCU2_PRODUCT.find("</Dynamic>").unwrap();
    let dynamic = format!(
        r#"<Dynamic><Channel Name="main" Text="Main" Id="{id}_CH-1">
          <ParameterBlock Id="{id}_PB-1" Name="general" Text="General">
            <ParameterRefRef RefId="{id}_P-9_R-9" />
          </ParameterBlock>
          <ParameterBlock Id="{id}_PB-9" Name="all" Text="Everything" Access="None">
            <ParameterRefRef RefId="{id}_P-1_R-1" />
            <ParameterRefRef RefId="{id}_P-7_R-7" />
            <ParameterSeparator Id="{id}_PS-1" Text="Not for anybody's eyes." />
            <choose ParamRefId="{id}_P-9_R-9">
              <when test="2"><ParameterRefRef RefId="{id}_P-4_R-4" /></when>
              <when default="true"><ParameterRefRef RefId="{id}_UP-2_R-2" /></when>
            </choose>
            <ParameterRefRef RefId="{id}_P-6_R-6" />
          </ParameterBlock>
          <ParameterBlock Id="{id}_PB-2" Name="dimming" ParamRefId="{id}_P-5_R-5" />
        </Channel>"#
    );
    let ordered = |product: String, reference: &str, more: &str| {
        let plain = format!(r#"<ParameterRef Id="{id}_{reference}""#);
        assert!(product.contains(&plain), "{reference}");
        product.replacen(&plain, &format!("{plain} {more}"), 1)
    };
    let mut product = format!(
        "{}{dynamic}{}",
        &BCU2_PRODUCT[..dynamic_from],
        &BCU2_PRODUCT[dynamic_to..]
    );
    // The page's title stands at 100; Spare at 110 and Level at 120 follow it. The read-only
    // parameter has no place in the order, and the one at 50 comes before every page.
    product = ordered(product, "P-5_R-5", r#"DisplayOrder="100" Text="Dimming""#);
    product = ordered(product, "P-4_R-4", r#"DisplayOrder="110""#);
    product = ordered(product, "P-1_R-1", r#"DisplayOrder="120""#);
    product = ordered(product, "P-7_R-7", r#"DisplayOrder="130""#);
    product = ordered(product, "UP-2_R-2", r#"DisplayOrder="50""#);
    let program = ApplicationProgram::parse(&product).unwrap();
    let baseline = DeviceData::parse(BCU2_DEVICE).unwrap();
    let addresses = GroupAddresses::parse_list(BCU2_ADDRESSES).unwrap();

    let seen = view(&program, &baseline, &baseline, &addresses).unwrap();
    let pages: Vec<(&str, Vec<&str>)> = seen
        .pages
        .iter()
        .map(|page| {
            let labels = page
                .items
                .iter()
                .map(|item| match item {
                    Item::Parameter(parameter) => parameter.label.as_str(),
                    Item::Text(text) => text.as_str(),
                })
                .collect();
            (page.title.as_str(), labels)
        })
        .collect();
    assert_eq!(
        pages,
        [
            ("General", vec!["Mode > all"]),
            ("Dimming", vec!["Spare", "Level"])
        ]
    );

    // Another mode: Spare is not in use any more, so it is gone from the page as well.
    let mut edits = Edits::default();
    edits
        .set_parameter(&program, &baseline, "P-9_R-9", "1")
        .unwrap();
    let (effective, known) = edits.apply(&program, &baseline, &addresses);
    let seen = view(&program, &baseline, &effective, &known).unwrap();
    assert_eq!(labels(&seen), ["Mode > all", "Level"]);
}

#[test]
fn texts_in_another_language() {
    let swedish = ApplicationProgram::parse_in(BCU2_PRODUCT, Some("sv-SE")).unwrap();
    let case = Case {
        program: swedish,
        ..bcu2()
    };
    let view = case.view(&Edits::default());
    assert_eq!(view.pages[0].title, "Allmänt");
    // Translated where the product has a translation, the product's own language elsewhere.
    assert_eq!(labels(&view)[..2], ["Nivå", "Mode > all"]);
    assert_eq!(view.objects[0].name, "Kanal A");
    let Control::Choice(choices) = &parameters(&view)[1].control else {
        panic!("a choice")
    };
    assert_eq!(choices[1].1, "Koppling");
    assert_eq!(choices[2].1, "Dimming");
    // The same language with another region, and a language the product does not have.
    let finnish_swedish = ApplicationProgram::parse_in(BCU2_PRODUCT, Some("sv-FI")).unwrap();
    assert_eq!(finnish_swedish.parameter("P-1").unwrap().text, "Nivå");
    let danish = ApplicationProgram::parse_in(BCU2_PRODUCT, Some("da-DK")).unwrap();
    assert_eq!(danish.parameter("P-1").unwrap().text, "Level");
}

#[test]
fn values_are_checked_against_the_type() {
    let program = ApplicationProgram::parse(BCU2_PRODUCT).unwrap();
    // Numbers: whole, within the limits, stored without decoration.
    assert_eq!(check_value(&program, "P-1_R-1", " 42 ").unwrap(), "42");
    assert_eq!(
        check_value(&program, "M-00FA_A-0102-03-0000_P-1_R-1", "0").unwrap(),
        "0"
    );
    for bad in ["256", "-1", "1.5", "", "ten"] {
        assert!(
            matches!(
                check_value(&program, "P-1_R-1", bad),
                Err(EditError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    assert_eq!(
        check_value(&program, "P-1_R-1", "300")
            .unwrap_err()
            .to_string(),
        "300 is outside 0 to 255"
    );
    // Enumerations: one of the members.
    assert_eq!(check_value(&program, "P-9_R-9", "3").unwrap(), "3");
    assert!(matches!(
        check_value(&program, "P-9_R-9", "4"),
        Err(EditError::Invalid(_))
    ));
    // Read-only, hidden, without a value: not for editing.
    for reference in ["P-6_R-6", "P-7_R-7", "P-5_R-5"] {
        assert!(
            matches!(
                check_value(&program, reference, "1"),
                Err(EditError::NotEditable(_))
            ),
            "{reference}"
        );
    }
    assert!(matches!(
        check_value(&program, "P-99_R-99", "1"),
        Err(EditError::UnknownParameter(_))
    ));

    // Text: as many characters as fit, and only those a device can hold. Signed numbers
    // within what their bits allow.
    let program = ApplicationProgram::parse(M112_PRODUCT).unwrap();
    assert_eq!(check_value(&program, "P-2_R-2", "abc").unwrap(), "abc");
    assert_eq!(check_value(&program, "P-2_R-2", "åä").unwrap(), "åä");
    assert!(check_value(&program, "P-2_R-2", "abcd").is_err());
    assert!(check_value(&program, "P-2_R-2", "€").is_err());
    assert_eq!(check_value(&program, "P-3_R-3", "-128").unwrap(), "-128");
    assert!(check_value(&program, "P-3_R-3", "128").is_err());
    assert!(check_value(&program, "P-1_R-1", "65535").is_ok());
    assert!(check_value(&program, "P-1_R-1", "65536").is_err());
}

#[test]
fn an_edit_changes_what_is_visible() {
    let case = bcu2();
    let mut edits = Edits::default();
    // Mode "Switching": the low nibble and object 2 come into use, the high nibble goes.
    edits
        .set_parameter(&case.program, &case.baseline, "P-9_R-9", "1")
        .unwrap();
    let view = case.view(&edits);
    assert_eq!(
        labels(&view),
        ["Level", "Mode > all", "Hardware revision", "Low"]
    );
    let mode = parameters(&view)[1];
    assert_eq!(mode.standing, Standing::Edited);
    assert_eq!(
        (mode.value_text.as_str(), mode.project_text.as_str()),
        ("Switching", "Dimming")
    );
    let numbers: Vec<u16> = view.objects.iter().map(|object| object.number).collect();
    assert_eq!(numbers, [0, 1, 2]);
    assert_eq!(view.objects[2].function, "Dimming");

    // Mode "Off": neither nibble, the spare parameter instead.
    edits
        .set_parameter(&case.program, &case.baseline, "P-9_R-9", "0")
        .unwrap();
    assert_eq!(
        labels(&case.view(&edits)),
        ["Level", "Mode > all", "Hardware revision", "Spare"]
    );

    // Back to the project's value: no edit is left.
    edits
        .set_parameter(&case.program, &case.baseline, "P-9_R-9", "2")
        .unwrap();
    assert!(edits.is_empty());
    assert_eq!(case.view(&edits), case.view(&Edits::default()));

    // A value that is refused changes nothing.
    assert!(
        edits
            .set_parameter(&case.program, &case.baseline, "P-1_R-1", "999")
            .is_err()
    );
    assert!(edits.is_empty());
}

/// What "changed" means for a device that was programmed with other values than the project
/// has: different from what it was programmed with.
#[test]
fn edits_are_measured_against_other_edits() {
    let case = bcu2();
    let relative = |now: &Edits, base: &Edits| {
        now.relative_to(&case.program, &case.baseline, &case.addresses, base)
            .unwrap()
    };
    let links = |edits: &mut Edits, links: &[u16]| {
        edits
            .set_links(
                &case.program,
                &case.baseline,
                &case.addresses,
                "O-0_R-1",
                links.iter().map(|raw| ga(*raw)).collect(),
            )
            .unwrap()
    };
    // Programmed with the mode "Switching" and a second address on object 0.
    let mut programmed = Edits::default();
    programmed
        .set_parameter(&case.program, &case.baseline, "P-9_R-9", "1")
        .unwrap();
    links(&mut programmed, &[2307, 2562]);
    assert!(relative(&programmed, &programmed).is_empty());
    assert_eq!(relative(&programmed, &Edits::default()), programmed);

    // Set back to what the project has: no edit over the project, and a change for the
    // device, to the project's value and links.
    let project = Edits::default();
    let back = relative(&project, &programmed);
    assert_eq!(back.parameters["P-9_R-9"], "2");
    assert_eq!(back.links["O-0_R-1"], [ga(2562)]);
    let (held, held_addresses) = programmed.apply(&case.program, &case.baseline, &case.addresses);
    let told = changes(&case.program, &held, &held_addresses, &back).unwrap();
    assert_eq!(
        (
            told.parameters[0].from.as_str(),
            told.parameters[0].to.as_str()
        ),
        ("Switching", "Dimming")
    );
    assert_eq!(
        (&told.links[0].added, &told.links[0].removed),
        (&vec![], &vec![ga(2307)])
    );

    // Another value still: one change, from the programmed one; the links are as programmed.
    let mut now = programmed.clone();
    now.set_parameter(&case.program, &case.baseline, "P-9_R-9", "0")
        .unwrap();
    let other = relative(&now, &programmed);
    assert_eq!(other.parameters.len(), 1);
    assert_eq!(other.parameters["P-9_R-9"], "0");
    assert!(other.links.is_empty());
}

#[test]
fn links_are_data() {
    let case = bcu2();
    let mut edits = Edits::default();
    let set = |edits: &mut Edits, reference: &str, links: &[u16]| {
        edits.set_links(
            &case.program,
            &case.baseline,
            &case.addresses,
            reference,
            links.iter().map(|raw| ga(*raw)).collect(),
        )
    };
    // A second address on object 0, the new one sending.
    set(&mut edits, "O-0_R-1", &[2307, 2562]).unwrap();
    let view = case.view(&edits);
    assert_eq!(view.objects[0].links, [ga(2307), ga(2562)]);
    let download = case.download(&edits);
    let tables = download.tables.as_ref().unwrap();
    assert_eq!(tables.addresses, [ga(2305), ga(2307), ga(2562)]);

    // The same address twice, an object the product does not have.
    assert_eq!(
        set(&mut edits, "O-0_R-1", &[2307, 2307]),
        Err(EditError::DuplicateLink(ga(2307)))
    );
    assert!(matches!(
        set(&mut edits, "O-9_R-9", &[2307]),
        Err(EditError::UnknownObject(_))
    ));

    // An object that goes out of use keeps its links, listed apart.
    set(&mut edits, "O-2_R-3", &[2306]).unwrap();
    let view = case.view(&edits);
    assert_eq!(view.unused.len(), 1);
    assert_eq!(
        (view.unused[0].number, view.unused[0].links.as_slice()),
        (2, &[ga(2306)][..])
    );
    edits
        .set_parameter(&case.program, &case.baseline, "P-9_R-9", "1")
        .unwrap();
    let view = case.view(&edits);
    assert!(view.unused.is_empty());
    assert_eq!(view.objects[2].links, [ga(2306)]);

    // What changed, in words.
    let summary = changes(&case.program, &case.baseline, &case.addresses, &edits).unwrap();
    assert_eq!(summary.parameters.len(), 1);
    let parameter = &summary.parameters[0];
    assert_eq!(
        (
            parameter.label.as_str(),
            parameter.from.as_str(),
            parameter.to.as_str()
        ),
        ("Mode > all", "Dimming", "Switching")
    );
    assert_eq!(summary.links.len(), 2);
    let first = &summary.links[0];
    assert_eq!((first.number, first.name.as_str()), (0, "Channel A"));
    assert_eq!(
        (first.added.as_slice(), first.removed.is_empty()),
        (&[ga(2307)][..], true)
    );
    assert!(first.sending_changed);
    assert_eq!(summary.links[1].added, [ga(2306)]);

    // Links as in the project are no edit; unlinking is one.
    set(&mut edits, "O-0_R-1", &[2562]).unwrap();
    assert!(!edits.links.contains_key("O-0_R-1"));
    set(&mut edits, "O-1_R-2", &[]).unwrap();
    assert_eq!(edits.links["O-1_R-2"], []);
    assert!(case.view(&edits).objects[1].links.is_empty());

    // More addresses than the product's table holds: the download cannot be prepared.
    let many: Vec<u16> = (3000..3007).collect();
    set(&mut edits, "O-0_R-1", &many).unwrap();
    let (effective, addresses) = edits.apply(&case.program, &case.baseline, &case.addresses);
    let error = Download::prepare(&case.program, &effective, &addresses).unwrap_err();
    assert!(
        matches!(error, knxide_program::Error::TableOverflow(_)),
        "{error}"
    );
}

#[test]
fn an_edit_changes_the_image_where_the_parameter_lives() {
    let case = bcu2();
    let before = case.download(&Edits::default());
    let mut edits = Edits::default();
    edits
        .set_parameter(&case.program, &case.baseline, "P-1_R-1", "77")
        .unwrap();
    let after = case.download(&edits);
    // "Level" is one byte at offset 11 of the segment at 0200.
    let mut changed = Vec::new();
    for segment in before.image.segments() {
        for offset in 0..segment.data.len() as u32 {
            let address = segment.address + offset;
            if before.image.byte(address) != after.image.byte(address) {
                changed.push(address);
            }
            assert_eq!(before.image.is_sent(address), after.image.is_sent(address));
        }
    }
    assert_eq!(changed, [0x020B]);
    assert_eq!(after.image.byte(0x020B), Some(77));
    assert_eq!(before.tables, after.tables);
    assert_eq!(before.procedure, after.procedure);
}

/// What a device that holds `device`'s download looks like to a comparison against `here`:
/// the stretches of `here`'s sent bytes that differ.
fn differences(here: &Download, device: &Download) -> Vec<Difference> {
    let mut out: Vec<Difference> = Vec::new();
    for run in here.image.runs(0, 0x1_0000) {
        for (offset, byte) in run.data.iter().enumerate() {
            let address = run.address + offset as u32;
            let held = device.image.byte(address).unwrap_or(0);
            if held == *byte {
                continue;
            }
            match out.last_mut() {
                Some(last) if u32::from(last.address) + last.device.len() as u32 == address => {
                    last.device.push(held);
                }
                _ => out.push(Difference {
                    address: address as u16,
                    device: vec![held],
                }),
            }
        }
    }
    out
}

#[test]
fn a_comparison_is_read_back_into_parameters_and_links() {
    let case = bcu2();
    let here = case.download(&Edits::default());
    let (effective, _) = Edits::default().apply(&case.program, &case.baseline, &case.addresses);

    // The device was loaded with another level, another nibble, and object 1 linked to
    // another address.
    let mut theirs = Edits::default();
    theirs
        .set_parameter(&case.program, &case.baseline, "P-1_R-1", "9")
        .unwrap();
    theirs
        .set_parameter(&case.program, &case.baseline, "UP-2_R-2", "3")
        .unwrap();
    theirs
        .set_links(
            &case.program,
            &case.baseline,
            &case.addresses,
            "O-1_R-2",
            vec![ga(2306)],
        )
        .unwrap();
    let device = case.download(&theirs);
    let found = decode(
        &case.program,
        &effective,
        &here,
        &differences(&here, &device),
        &[],
    )
    .unwrap();

    let mut parameters = Vec::new();
    let mut links = Vec::new();
    let mut bytes = Vec::new();
    for finding in &found {
        match finding {
            Finding::Parameter {
                reference,
                label,
                here_text,
                device,
                device_text,
                adoptable,
                ..
            } => parameters.push((
                reference.as_str(),
                label.as_str(),
                here_text.as_str(),
                device.as_str(),
                device_text.as_str(),
                *adoptable,
            )),
            Finding::Links {
                reference,
                number,
                here,
                device,
                ..
            } => links.push((reference.as_deref(), *number, here.clone(), device.clone())),
            Finding::Bytes { address, what, .. } => bytes.push((*address, what.clone())),
        }
    }
    assert_eq!(
        parameters,
        [
            ("P-1_R-1", "Level", "6 %", "9", "9 %", true),
            ("UP-2_R-2", "High", "nine", "3", "three", true),
        ]
    );
    assert_eq!(
        links,
        [(Some("O-1_R-2"), 1, vec![ga(2305)], vec![ga(2306)])]
    );
    // Nothing is left over as bytes: every difference means one of the three.
    assert_eq!(bytes, []);

    // Adopting what was found makes the plan equal to the device.
    let mut adopted = Edits::default();
    for finding in &found {
        match finding {
            Finding::Parameter {
                reference, device, ..
            } => adopted
                .set_parameter(&case.program, &case.baseline, reference, device)
                .unwrap(),
            Finding::Links {
                reference: Some(reference),
                device,
                ..
            } => adopted
                .set_links(
                    &case.program,
                    &case.baseline,
                    &case.addresses,
                    reference,
                    device.clone(),
                )
                .unwrap(),
            _ => {}
        }
    }
    assert_eq!(adopted, theirs);
    assert!(differences(&case.download(&adopted), &device).is_empty());
}

/// A device that holds another number of group addresses than the plan has its association
/// table elsewhere (on this mask it follows the address table), partly where the download
/// sends nothing. The comparison is told which bytes it still needs, and with those the
/// difference reads in group addresses.
#[test]
fn tables_of_another_shape_are_read_with_what_lies_beside_the_plan() {
    let case = bcu2();
    // Here: object 0 linked to a second address. The device: as the project has it.
    let mut edits = Edits::default();
    edits
        .set_links(
            &case.program,
            &case.baseline,
            &case.addresses,
            "O-0_R-1",
            vec![ga(2562), ga(2049)],
        )
        .unwrap();
    let here = case.download(&edits);
    let (effective, _) = edits.apply(&case.program, &case.baseline, &case.addresses);
    let device = case.download(&Edits::default());
    let seen = differences(&here, &device);

    // Without more, the tables cannot be read back: the differences stay bytes with notes.
    let found = decode(&case.program, &effective, &here, &seen, &[]).unwrap();
    assert!(
        found
            .iter()
            .all(|finding| matches!(finding, Finding::Bytes { what: Some(_), .. })),
        "{found:?}"
    );

    // What is missing lies inside the tables, where the download sends nothing.
    let tables = here.tables.as_ref().unwrap();
    let mut beside = Vec::new();
    for round in 0.. {
        let gaps = table_gaps(&here, &seen, &beside);
        if gaps.is_empty() {
            break;
        }
        assert!(round < 3, "the gaps close: {gaps:?}");
        for (address, length) in gaps {
            let bytes: Vec<u8> = (0..length as u32)
                .map(|offset| {
                    let at = u32::from(address) + offset;
                    assert!((tables.address_table..tables.association_table + 16).contains(&at));
                    assert!(!here.image.is_sent(at), "{at:04X} is compared already");
                    device.image.byte(at).unwrap()
                })
                .collect();
            beside.push(Difference {
                address,
                device: bytes,
            });
        }
    }
    assert!(!beside.is_empty());

    let found = decode(&case.program, &effective, &here, &seen, &beside).unwrap();
    assert_eq!(
        found,
        [Finding::Links {
            reference: Some("O-0_R-1".into()),
            number: 0,
            name: "Channel A".into(),
            here: vec![ga(2562), ga(2049)],
            device: vec![ga(2562)],
        }]
    );
    // A device that agrees with the plan needs nothing more.
    assert!(table_gaps(&here, &[], &[]).is_empty());
}

#[test]
fn differences_that_mean_nothing_known_stay_bytes() {
    let case = m112();
    let here = case.download(&Edits::default());
    let (effective, _) = Edits::default().apply(&case.program, &case.baseline, &case.addresses);
    // A text, a signed number and a byte no parameter owns.
    let found = decode(
        &case.program,
        &effective,
        &here,
        &[
            Difference {
                address: 0x440D,
                device: b"Yo\0".to_vec(),
            },
            Difference {
                address: 0x4410,
                device: vec![0x80],
            },
            Difference {
                address: 0x4412,
                device: vec![0x55, 0x66],
            },
        ],
        &[],
    )
    .unwrap();
    let summary: Vec<String> = found
        .iter()
        .map(|finding| match finding {
            Finding::Parameter {
                reference, device, ..
            } => format!("{reference}={device}"),
            Finding::Links { number, .. } => format!("links of {number}"),
            Finding::Bytes {
                address, device, ..
            } => format!("{address:04X}:{device:02X?}"),
        })
        .collect();
    assert_eq!(summary, ["P-2_R-2=Yo", "P-3_R-3=-128", "4412:[55, 66]"]);

    // A device with more group addresses than the plan: the tables cannot be read back
    // from what was compared, and the difference stays bytes with a note.
    let tables = here.tables.as_ref().unwrap();
    let found = decode(
        &case.program,
        &effective,
        &here,
        &[Difference {
            address: tables.address_table as u16,
            device: vec![5],
        }],
        &[],
    )
    .unwrap();
    assert_eq!(
        found,
        [Finding::Bytes {
            address: tables.address_table as u16,
            here: vec![2],
            device: vec![5],
            what: Some("group address table".into()),
        }]
    );
}

#[test]
fn project_devices_can_be_edited() {
    // The same through a project: the baseline comes from the project, the group addresses
    // with it.
    let bytes = archive();
    let project = Project::load(&ProjectArchive::open(&bytes).unwrap()).unwrap();
    let device = &project.devices[0];
    let program = device.application.as_ref().unwrap();
    let mut edits = Edits::default();
    edits
        .set_parameter(program, &device.data, "P-9_R-9", "3")
        .unwrap();
    edits
        .set_links(
            program,
            &device.data,
            &project.addresses,
            "O-1_R-2",
            vec![ga(2306), ga(2305)],
        )
        .unwrap();
    let (effective, addresses) = edits.apply(program, &device.data, &project.addresses);
    let view = view(program, &device.data, &effective, &addresses).unwrap();
    assert_eq!(view.objects[1].links, [ga(2306), ga(2305)]);
    let download = Download::prepare(program, &effective, &addresses).unwrap();
    assert_eq!(download.tables.unwrap().addresses.len(), 3);
    assert_eq!(
        project.group_address(ga(2306)).unwrap().name,
        "Hall light brightness"
    );
    // The phases a download of it goes through, in words.
    let phases = device.download.as_ref().unwrap().phases();
    assert_eq!(
        phases,
        [
            "Checking the device",
            "Device settings",
            "Unloading",
            "Address table",
            "Association table",
            "Application and parameters",
            "Restart"
        ]
    );
}

// ------------------------------------------------------------------------------------------
// The recorded devices
// ------------------------------------------------------------------------------------------

fn recordings() -> Vec<PathBuf> {
    let Some(root) = std::env::var_os("KNXIDE_PROGRAM_FIXTURES").map(PathBuf::from) else {
        eprintln!("skipped: KNXIDE_PROGRAM_FIXTURES is not set");
        return Vec::new();
    };
    let mut directories: Vec<PathBuf> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.join("app.xml").is_file())
        .collect();
    directories.sort();
    directories
}

/// Every address of the image where `before` and `after` differ in content or in whether
/// the byte is sent.
fn changed_addresses(before: &Download, after: &Download) -> Vec<u32> {
    let mut changed = Vec::new();
    for segment in before.image.segments() {
        for offset in 0..segment.data.len() as u32 {
            let address = segment.address + offset;
            if before.image.byte(address) != after.image.byte(address)
                || before.image.is_sent(address) != after.image.is_sent(address)
            {
                changed.push(address);
            }
        }
    }
    changed
}

/// Start from each recorded device, change one enumeration parameter and one link, and check
/// that the image and the tables change exactly where they should and nowhere else.
#[test]
fn edits_to_recorded_devices_change_exactly_what_they_should() {
    for directory in recordings() {
        let name = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let read = |file: &str| std::fs::read_to_string(directory.join(file)).unwrap();
        let program = ApplicationProgram::parse(&read("app.xml")).unwrap();
        let baseline = DeviceData::parse(&read("device.xml")).unwrap();
        let addresses = GroupAddresses::parse_list(&read("group-addresses.txt")).unwrap();
        let before = Download::prepare(&program, &baseline, &addresses).unwrap();
        let configuration = Configuration::evaluate(&program, &baseline).unwrap();

        // An enumeration parameter that is in use, lives in memory, can be edited, and does
        // not decide what else is visible: one whose value is no `choose` condition.
        let view_before = view(&program, &baseline, &baseline, &addresses).unwrap();
        let visible: Vec<&ParameterView> = parameters(&view_before);
        let steers = |reference: &str| {
            fn any(nodes: &[knxide_program::product::DynamicNode], reference: &str) -> bool {
                use knxide_program::product::DynamicNode;
                nodes.iter().any(|node| match node {
                    DynamicNode::Choose {
                        parameter_ref,
                        branches,
                    } => {
                        parameter_ref == reference
                            || branches
                                .iter()
                                .any(|branch| any(&branch.children, reference))
                    }
                    DynamicNode::Channel { children, .. }
                    | DynamicNode::Block { children, .. }
                    | DynamicNode::Group(children) => any(children, reference),
                    _ => false,
                })
            }
            any(&program.dynamic, reference)
        };
        let candidate = visible.iter().find_map(|parameter| {
            let Control::Choice(choices) = &parameter.control else {
                return None;
            };
            if steers(&parameter.reference) {
                return None;
            }
            let at = program.parameter_ref_index(&parameter.reference)?;
            let definition = program.parameter(&program.parameter_refs[at].parameter)?;
            let ParameterLocation::Memory { segment, bit } = &definition.location else {
                return None;
            };
            let kind = program.parameter_type(&definition.parameter_type)?;
            if kind.kind != ParameterKind::Restriction {
                return None;
            }
            // No other parameter in use may share its bytes, so that the expectation below
            // is plain.
            let other = choices
                .iter()
                .map(|(value, _)| value.clone())
                .find(|value| *value != parameter.value)?;
            let start = u64::from(program.segment(segment)?.address) * 8 + u64::from(*bit);
            Some((
                parameter.reference.clone(),
                other,
                start,
                kind.size_in_bit,
                at,
            ))
        });
        let Some((reference, value, first_bit, bits, index)) = candidate else {
            panic!("{name}: no enumeration parameter to edit");
        };
        assert!(configuration.parameter_active(index));

        let mut edits = Edits::default();
        edits
            .set_parameter(&program, &baseline, &reference, &value)
            .unwrap();
        let (effective, known) = edits.apply(&program, &baseline, &addresses);
        let after = Download::prepare(&program, &effective, &known).unwrap();
        // Exactly the bytes of the parameter's field changed, and they hold the new value.
        let field: Vec<u32> =
            ((first_bit / 8) as u32..(first_bit + u64::from(bits)).div_ceil(8) as u32).collect();
        let changed = changed_addresses(&before, &after);
        assert!(!changed.is_empty(), "{name}: {reference} changed nothing");
        assert!(
            changed.iter().all(|address| field.contains(address)),
            "{name}: {reference} changed {changed:04X?}, its field is {field:04X?}"
        );
        let mut stored = 0u64;
        for position in first_bit..first_bit + u64::from(bits) {
            let byte = after.image.byte((position / 8) as u32).unwrap();
            stored = stored << 1 | u64::from(byte >> (7 - position % 8) & 1);
        }
        assert_eq!(stored.to_string(), value, "{name}: {reference}");
        assert_eq!(before.tables, after.tables, "{name}");
        assert_eq!(before.procedure, after.procedure, "{name}");
        // The view shows it as edited, and nothing appeared or disappeared.
        let view_after = view(&program, &baseline, &effective, &known).unwrap();
        assert_eq!(
            labels(&view_before).len(),
            labels(&view_after).len(),
            "{name}"
        );
        let edited: Vec<&str> = parameters(&view_after)
            .iter()
            .filter(|parameter| parameter.standing == Standing::Edited)
            .map(|parameter| parameter.reference.as_str())
            .collect();
        assert_eq!(edited, [reference.as_str()], "{name}");
        println!(
            "{name}: {reference} -> {value}: image changed at {changed:04X?}, tables unchanged"
        );

        // One link: the first linked object gets one address more, an address no object has.
        let object = view_before
            .objects
            .iter()
            .find(|object| !object.links.is_empty())
            .unwrap_or_else(|| panic!("{name}: no linked object"));
        let tables_before = before.tables.as_ref().unwrap();
        let fresh = (1..u16::MAX)
            .map(ga)
            .find(|address| !tables_before.addresses.contains(address))
            .unwrap();
        let mut links = object.links.clone();
        links.push(fresh);
        let mut edits = Edits::default();
        edits
            .set_links(
                &program,
                &baseline,
                &addresses,
                &object.reference,
                links.clone(),
            )
            .unwrap();
        let (effective, known) = edits.apply(&program, &baseline, &addresses);
        let after = Download::prepare(&program, &effective, &known).unwrap();
        let tables_after = after.tables.as_ref().unwrap();
        // The address table: the old addresses and the new one, ascending.
        let mut expected = tables_before.addresses.clone();
        expected.push(fresh);
        expected.sort();
        assert_eq!(tables_after.addresses, expected, "{name}");
        // The association table: one entry more for this object, to the new address; every
        // other object keeps its addresses.
        let resolve = |tables: &knxide_program::Tables| -> Vec<(u8, GroupAddress)> {
            let mut pairs: Vec<(u8, GroupAddress)> = tables
                .associations
                .iter()
                .filter_map(|association| {
                    let address = tables
                        .addresses
                        .get(usize::from(association.connection).checked_sub(1)?)?;
                    Some((association.object, *address))
                })
                .collect();
            pairs.sort();
            pairs
        };
        let mut expected_pairs = resolve(tables_before);
        expected_pairs.push((object.number as u8, fresh));
        expected_pairs.sort();
        assert_eq!(resolve(tables_after), expected_pairs, "{name}");
        // The objects themselves: the same, but for this one's links.
        for (old, new) in tables_before.objects.iter().zip(&tables_after.objects) {
            if old.number == object.number {
                assert_eq!(new.links, links, "{name}");
            } else {
                assert_eq!(old, new, "{name}");
            }
        }
        // In the image, only table bytes changed: nothing in the object table or behind it
        // apart from the tables' own places.
        let object_table = tables_after.object_table;
        let table_end = |tables: &knxide_program::Tables| {
            (tables.association_table + 2 + 2 * tables.associations.len() as u32)
                .max(tables.address_table + 4 + 2 * tables.addresses.len() as u32)
        };
        let end = table_end(tables_before).max(table_end(tables_after));
        let start = tables_after
            .address_table
            .min(tables_after.association_table);
        let changed = changed_addresses(&before, &after);
        assert!(!changed.is_empty(), "{name}");
        let stray: Vec<u32> = changed
            .iter()
            .copied()
            .filter(|address| {
                let in_tables = (start..end).contains(address);
                // BCU 2 keeps the low byte of the association table's address at 0111.
                let pointer = *address == 0x0111;
                !(in_tables || pointer)
            })
            .collect();
        assert!(
            stray.is_empty(),
            "{name}: bytes outside the tables changed: {stray:04X?} (object table at \
             {object_table:04X})"
        );
        println!(
            "{name}: object {} linked to one address more: {} bytes changed, all within the \
             address and association tables",
            object.number,
            changed.len()
        );
    }
}
