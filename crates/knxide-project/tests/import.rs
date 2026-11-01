//! Import ETS exports in each format.
//!
//! The samples follow ETS 5/6 exports; layouts are as ETS writes them, contents are made up.

use knxide_core::dpt::DptId;
use knxide_core::ga;
use knxide_project::{GroupAddressTable, ImportError};

const CSV_1_1: &str = "\u{FEFF}\"Group name\";\"Address\";\"Central\";\"Unfiltered\";\"Description\";\"DatapointType\";\"Security\"
\"Kitchen light\";\"1/0/1\";\"\";\"\";\"Ceiling, dimmable\";\"DPST-1-1\";\"Auto\"
\"Kitchen dim\";\"1/0/2\";\"\";\"\";\"\";\"DPST-3-7\";\"Auto\"
\"Living temperature\";\"3/1/0\";\"\";\"\";\"\";\"DPST-9-1\";\"Auto\"
\"Spare\";\"3/1/1\";\"\";\"\";\"\";\"\";\"Auto\"
\"Power\";\"4/0/0\";\"\";\"\";\"\";\"DPT-14\";\"Auto\"
";

const CSV_3_1: &str = "\"Main\";\"Middle\";\"Sub\";\"Address\";\"Central\";\"Unfiltered\";\"Description\";\"DatapointType\";\"Security\"
\"Lighting\";\"\";\"\";\"1/-/-\";\"\";\"\";\"\";\"\";\"Auto\"
\"\";\"Kitchen\";\"\";\"1/0/-\";\"\";\"\";\"\";\"\";\"Auto\"
\"\";\"\";\"Kitchen light\";\"1/0/1\";\"\";\"\";\"\";\"DPST-1-1\";\"Auto\"
\"Heating\";\"\";\"\";\"3/-/-\";\"\";\"\";\"\";\"\";\"Auto\"
\"\";\"Living\";\"\";\"3/1/-\";\"\";\"\";\"\";\"\";\"Auto\"
\"\";\"\";\"Living temperature\";\"3/1/0\";\"\";\"\";\"Sensor\";\"DPST-9-1\";\"Auto\"
";

const CSV_GERMAN_TABS: &str =
    "Gruppenname\tAdresse\tZentral\tDurchgangsfunktion\tBeschreibung\tDatenpunkttyp\tSicherheit
Küche Licht\t1/0/1\t\t\tDecke\tDPST-1-1\tAuto
";

const CSV_NO_HEADER: &str = "\"Kitchen light\",\"1/0/1\",\"\",\"\",\"\",\"DPST-1-1\",\"Auto\"
\"Living temperature\",\"3/1/0\",\"\",\"\",\"\",\"DPST-9-1\",\"Auto\"
";

const XML: &str = r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<GroupAddress-Export xmlns="http://knx.org/xml/ga-export/01">
  <!-- exported by ETS -->
  <GroupRange Name="Lighting" RangeStart="2048" RangeEnd="4095">
    <GroupRange Name="Kitchen &amp; dining" RangeStart="2048" RangeEnd="2303">
      <GroupAddress Name="Kitchen light" Address="1/0/1" DPTs="DPST-1-1" Description="Ceiling" />
      <GroupAddress Name="Kitchen dim" Address="1/0/2" DPTs="DPST-3-7,DPST-3-8" />
    </GroupRange>
  </GroupRange>
  <GroupRange Name="Heating" RangeStart="6144" RangeEnd="8191">
    <GroupRange Name="Living" RangeStart="6400" RangeEnd="6655">
      <GroupAddress Name="Living temperature" Address="3/1/0" DPTs="DPST-9-1" />
      <GroupAddress Name="Untyped" Address="3/1/1" />
    </GroupRange>
  </GroupRange>
</GroupAddress-Export>
"#;

#[test]
fn csv_1_1() {
    let table = GroupAddressTable::parse(CSV_1_1).unwrap();
    assert_eq!(table.len(), 5);
    let light = table.get(ga!(1 / 0 / 1)).unwrap();
    assert_eq!(light.name, "Kitchen light");
    assert_eq!(light.description, "Ceiling, dimmable");
    assert_eq!(light.dpt, Some(DptId::new(1, 1)));
    assert!(light.path.is_empty());
    assert_eq!(table.dpt(ga!(1 / 0 / 2)), Some(DptId::new(3, 7)));
    assert_eq!(table.dpt(ga!(3 / 1 / 0)), Some(DptId::new(9, 1)));
    assert_eq!(table.dpt(ga!(3 / 1 / 1)), None);
    assert_eq!(table.dpt(ga!(4 / 0 / 0)), Some(DptId::main(14)));
}

#[test]
fn csv_3_1_keeps_group_ranges() {
    let table = GroupAddressTable::parse(CSV_3_1).unwrap();
    assert_eq!(table.len(), 2);
    let light = table.get(ga!(1 / 0 / 1)).unwrap();
    assert_eq!(light.name, "Kitchen light");
    assert_eq!(light.path, ["Lighting", "Kitchen"]);
    let temp = table.get(ga!(3 / 1 / 0)).unwrap();
    assert_eq!(temp.path, ["Heating", "Living"]);
    assert_eq!(temp.description, "Sensor");
    assert_eq!(temp.dpt, Some(DptId::new(9, 1)));
}

#[test]
fn csv_german_with_tabs() {
    let table = GroupAddressTable::parse(CSV_GERMAN_TABS).unwrap();
    let light = table.get(ga!(1 / 0 / 1)).unwrap();
    assert_eq!(light.name, "Küche Licht");
    assert_eq!(light.description, "Decke");
    assert_eq!(light.dpt, Some(DptId::new(1, 1)));
}

#[test]
fn csv_without_header() {
    let table = GroupAddressTable::parse(CSV_NO_HEADER).unwrap();
    assert_eq!(table.len(), 2);
    assert_eq!(table.get(ga!(1 / 0 / 1)).unwrap().name, "Kitchen light");
    assert_eq!(table.dpt(ga!(3 / 1 / 0)), Some(DptId::new(9, 1)));
}

#[test]
fn csv_latin1_bytes() {
    let mut bytes = b"Gruppenname;Adresse;Datenpunkttyp\n".to_vec();
    bytes.extend_from_slice(b"K\xFCche;1/0/1;DPST-1-1\n");
    let table = GroupAddressTable::parse_bytes(&bytes).unwrap();
    assert_eq!(table.get(ga!(1 / 0 / 1)).unwrap().name, "Küche");
}

#[test]
fn utf16_bytes() {
    let text = "Group name;Address;DatapointType\nLight;1/0/1;DPST-1-1\n";
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let table = GroupAddressTable::parse_bytes(&bytes).unwrap();
    assert_eq!(table.dpt(ga!(1 / 0 / 1)), Some(DptId::new(1, 1)));
}

#[test]
fn xml() {
    let table = GroupAddressTable::parse(XML).unwrap();
    assert_eq!(table.len(), 4);
    let light = table.get(ga!(1 / 0 / 1)).unwrap();
    assert_eq!(light.name, "Kitchen light");
    assert_eq!(light.description, "Ceiling");
    assert_eq!(light.path, ["Lighting", "Kitchen & dining"]);
    assert_eq!(light.dpt, Some(DptId::new(1, 1)));
    // Several DPTs: the first one wins.
    assert_eq!(table.dpt(ga!(1 / 0 / 2)), Some(DptId::new(3, 7)));
    assert_eq!(
        table.get(ga!(3 / 1 / 0)).unwrap().path,
        ["Heating", "Living"]
    );
    assert_eq!(table.dpt(ga!(3 / 1 / 1)), None);
    let addresses: Vec<String> = table.iter().map(|e| e.address.to_string()).collect();
    assert_eq!(addresses, ["1/0/1", "1/0/2", "3/1/0", "3/1/1"]);
}

#[test]
fn rejects_other_input() {
    assert_eq!(GroupAddressTable::parse(""), Err(ImportError::Empty));
    assert_eq!(
        GroupAddressTable::parse("hello;world\nfoo;bar\n"),
        Err(ImportError::UnknownFormat)
    );
    assert_eq!(
        GroupAddressTable::parse("<html><body/></html>"),
        Err(ImportError::UnknownFormat)
    );
    assert!(matches!(
        GroupAddressTable::parse("<GroupAddress-Export><GroupAddress Address=\"99/0/0\"/>"),
        Err(ImportError::Xml(_))
    ));
    assert!(matches!(
        GroupAddressTable::parse("<GroupAddress-Export><a b=c/>"),
        Err(ImportError::Xml(_))
    ));
}
