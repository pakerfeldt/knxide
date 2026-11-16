//! Whole downloads of made-up products, one per supported mask, with expectations worked out
//! by hand from the rules. No manufacturer data is involved; every identifier, address and
//! value below is invented.

use std::time::Duration;

use knxide_program::plan::{PlanError, PlanFailure};
use knxide_program::{
    ApplicationProgram, DeviceData, Download, DownloadKind, GroupAddresses, Plan, PlanOptions,
    Request, Response, SimulatedDevice,
};
use knxide_program::{Device, Expect, Fault};
use knxide_testkit::project::{
    BCU2_ADDRESSES, BCU2_DEVICE, BCU2_PRODUCT, M112_DEVICE, M112_PRODUCT, PARTIAL_DEVICE,
    PARTIAL_PRODUCT,
};

fn lines(requests: &[Request]) -> Vec<String> {
    requests.iter().map(ToString::to_string).collect()
}

fn build(product: &str, device: &str, addresses: &str) -> Download {
    let program = ApplicationProgram::parse(product).unwrap();
    let device = DeviceData::parse(device).unwrap();
    let addresses = GroupAddresses::parse_list(addresses).unwrap();
    Download::prepare(&program, &device, &addresses).unwrap()
}

/// A modelled device that holds the application of `download` and passes its pre-check.
fn holding(download: &Download) -> SimulatedDevice {
    SimulatedDevice::holding(download)
}

/// The requests of the pre-check, by mask: what follows the four requests of the opening.
const SERIAL: &str = "PROP_READ obj=0 pid=11 start=1 count=1";
const MANUFACTURER: &str = "PROP_READ obj=0 pid=12 start=1 count=1";

fn image_bytes(download: &Download, address: u32, count: u32) -> Vec<u8> {
    (address..address + count)
        .map(|at| download.image.byte(at).unwrap())
        .collect()
}

fn sent(download: &Download, address: u32, count: u32) -> Vec<u32> {
    (address..address + count)
        .filter(|at| download.image.is_sent(*at))
        .collect()
}

// ------------------------------------------------------------------------------------------
// BCU 2 (mask 0021): dynamic tables, property load control, echoed memory writes.
// ------------------------------------------------------------------------------------------

#[test]
fn bcu2_image_and_tables() {
    let download = build(BCU2_PRODUCT, BCU2_DEVICE, BCU2_ADDRESSES);
    assert_eq!(download.kind, DownloadKind::FULL);
    assert_eq!(download.configuration.active_parameter_count(), 6);
    assert_eq!(download.configuration.active_object_count(), 2);

    let tables = download.tables.as_ref().unwrap();
    assert_eq!(tables.address_table, 0x0116);
    let raw: Vec<u16> = tables.addresses.iter().map(|a| a.raw()).collect();
    assert_eq!(raw, [0x0901, 0x0A02]);
    // 3 + 2 * 2 bytes of table, one extra byte, then the association table.
    assert_eq!(tables.association_table, 0x011E);
    let placement = tables.placement.unwrap();
    assert_eq!(placement.address_table_size, 8);

    // Address table: count + 1, the untouched individual address slot, the addresses.
    assert_eq!(image_bytes(&download, 0x0116, 1), [0x03]);
    assert_eq!(image_bytes(&download, 0x0119, 4), [0x09, 0x01, 0x0A, 0x02]);
    // Association table: one slot per object; object 2 has no address.
    assert_eq!(
        image_bytes(&download, 0x011E, 7),
        [0x03, 0x02, 0x00, 0x01, 0x01, 0xFE, 0x02]
    );
    // Only the table bytes of the two reservations are sent.
    assert_eq!(
        sent(&download, 0x0116, 28),
        [
            0x0116, 0x0119, 0x011A, 0x011B, 0x011C, 0x011E, 0x011F, 0x0120, 0x0121, 0x0122, 0x0123,
            0x0124
        ]
    );

    // Object table: object 0 (linked), object 1 (linked; read flag now set, top type bits kept),
    // object 2 (not in use: rebuilt without the communication bit, type untouched).
    assert_eq!(
        image_bytes(&download, 0x0200, 11),
        [
            0x03, 0xE0, 0xE1, 0x37, 0x00, 0xE2, 0x6F, 0x47, 0xE3, 0x1B, 0x03
        ]
    );
    // Parameters: level 6 (reference default), high nibble 9 with the product's low nibble,
    // an uncovered product byte, then a hidden parameter on a byte the product masks out.
    assert_eq!(image_bytes(&download, 0x020B, 4), [0x06, 0x9F, 0xFF, 0x00]);
    assert_eq!(
        sent(&download, 0x0200, 16),
        (0x0200..0x020F).collect::<Vec<_>>()
    );

    // Memory header: masked product bytes, application identity, interface type, pointer to the
    // association table.
    assert_eq!(
        sent(&download, 0x0100, 22),
        [
            0x0100, 0x0103, 0x0104, 0x0105, 0x0106, 0x0107, 0x0109, 0x010E, 0x010F, 0x0110, 0x0111
        ]
    );
    assert_eq!(
        image_bytes(&download, 0x0103, 5),
        [0x00, 0xFA, 0x01, 0x02, 0x03]
    );
    assert_eq!(image_bytes(&download, 0x0111, 1), [0x1E]);
}

#[test]
fn bcu2_requests() {
    let download = build(BCU2_PRODUCT, BCU2_DEVICE, BCU2_ADDRESSES);
    let mut device = holding(&download);
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    let expected = [
        // Opening.
        "T_CONNECT",
        "DEVICE_DESCRIPTOR_READ",
        "PROP_READ obj=0 pid=56 start=1 count=1",
        "AUTHORIZE_REQUEST FFFFFFFF",
        // Pre-check of a full download: serial number, manufacturer, hardware.
        SERIAL,
        MANUFACTURER,
        "PROP_READ obj=0 pid=17 start=1 count=1",
        // Ten inline bytes, two of them sent: the property holds two.
        "PROP_WRITE obj=0 pid=8 start=1 count=1 0000",
        "PROP_WRITE obj=1 pid=5 start=1 count=1 04000000000000000000",
        "PROP_WRITE obj=2 pid=5 start=1 count=1 04000000000000000000",
        "PROP_WRITE obj=3 pid=5 start=1 count=1 04000000000000000000",
        // Address table: segment shrunk to 8 bytes, verify mode switched on, table written.
        "PROP_WRITE obj=1 pid=5 start=1 count=1 01000000000000000000",
        "PROP_WRITE obj=1 pid=5 start=1 count=1 03000116000831038000",
        "PROP_READ obj=0 pid=14 start=1 count=1",
        "PROP_READ obj=0 pid=14 start=1 count=1",
        "PROP_WRITE obj=0 pid=14 start=1 count=1 04",
        "PROP_READ obj=0 pid=14 start=1 count=1",
        "MEM_WRITE 0116 03",
        "MEM_WRITE 0119 09010A02",
        "PROP_WRITE obj=1 pid=5 start=1 count=1 030201160000FA010203",
        "PROP_WRITE obj=1 pid=5 start=1 count=1 02000000000000000000",
        // Association table: moved to 011E, keeps its old end (0132).
        "PROP_WRITE obj=2 pid=5 start=1 count=1 01000000000000000000",
        "PROP_WRITE obj=2 pid=5 start=1 count=1 0300011E001431038000",
        "MEM_WRITE 011E 0302000101FE02",
        "PROP_WRITE obj=2 pid=5 start=1 count=1 0302011E0000FA010203",
        "PROP_WRITE obj=2 pid=5 start=1 count=1 02000000000000000000",
        // Application.
        "PROP_WRITE obj=3 pid=5 start=1 count=1 01000000000000000000",
        "PROP_WRITE obj=3 pid=5 start=1 count=1 03000100001630030000",
        "MEM_WRITE 0100 AA",
        "MEM_WRITE 0103 00FA010203",
        "MEM_WRITE 0109 00",
        "MEM_WRITE 010E 1122331E",
        "PROP_WRITE obj=3 pid=5 start=1 count=1 03000200001030038000",
        "MEM_WRITE 0200 03E0E13700E26F47E31B0306",
        "MEM_WRITE 020C 9FFF00",
        // A RAM segment is allocated but not written.
        "PROP_WRITE obj=3 pid=5 start=1 count=1 03000BB8000A30010000",
        "PROP_WRITE obj=3 pid=5 start=1 count=1 030202000000FA010203",
        "PROP_WRITE obj=3 pid=5 start=1 count=1 03030201020200000000",
        "PROP_WRITE obj=3 pid=5 start=1 count=1 03040203010000000000",
        "PROP_WRITE obj=3 pid=5 start=1 count=1 0305123402000BB80000",
        "PROP_WRITE obj=3 pid=5 start=1 count=1 02000000000000000000",
        "RESTART",
        "T_DISCONNECT",
    ];
    assert_eq!(lines(&plan.requests), expected);
    assert_eq!(plan.apdu_length, 15);
    assert_eq!(plan.stamp_matches, None);
    // What the device ends up with is what the image says.
    assert_eq!(device.memory(0x020C), 0x9F);
    assert_eq!(device.memory(0x0111), 0x1E);
}

#[test]
fn bcu2_other_selection_and_no_links() {
    // Product default mode 1: object 2 and the low nibble come into use, nothing is linked.
    let device = r#"<DeviceInstance Address="9" />"#;
    let download = build(BCU2_PRODUCT, device, "");
    assert_eq!(download.configuration.active_object_count(), 3);
    let tables = download.tables.as_ref().unwrap();
    assert!(tables.addresses.is_empty());
    assert_eq!(tables.association_table, 0x011A);
    assert_eq!(image_bytes(&download, 0x0116, 1), [0x01]);
    assert_eq!(
        image_bytes(&download, 0x011A, 7),
        [0x03, 0xFE, 0x00, 0xFE, 0x01, 0xFE, 0x02]
    );
    // In use but unlinked: flags without the communication bit, size code set (4 bit = 3).
    assert_eq!(image_bytes(&download, 0x0209, 2), [0x1B, 0x03]);
    assert_eq!(image_bytes(&download, 0x0203, 2), [0x33, 0x00]);
    // Low nibble 3 written, high nibble keeps the product's 0.
    assert_eq!(image_bytes(&download, 0x020C, 1), [0x03]);
}

#[test]
fn bcu2_unknown_group_address_is_an_error() {
    let program = ApplicationProgram::parse(BCU2_PRODUCT).unwrap();
    let device = DeviceData::parse(BCU2_DEVICE).unwrap();
    let addresses = GroupAddresses::parse_list("GA-1 2305\n").unwrap();
    let result = Download::build(&program, &device, &addresses, DownloadKind::FULL);
    assert!(matches!(
        result,
        Err(knxide_program::Error::UnknownReference(id)) if id == "GA-2"
    ));
}

#[test]
fn bcu2_table_overflow_is_an_error() {
    // Seven addresses need 3 + 14 + 1 bytes for the address table alone, and the association
    // table comes on top; 15 + 11 bytes are reserved for both.
    let mut links = String::new();
    let mut list = String::new();
    for n in 1..=7 {
        links.push_str(&format!("GA-{n} "));
        list.push_str(&format!("GA-{n} {}\n", 2000 + n));
    }
    let device = format!(
        r#"<DeviceInstance><ComObjectInstanceRefs>
             <ComObjectInstanceRef RefId="O-0_R-1" Links="{links}" />
           </ComObjectInstanceRefs></DeviceInstance>"#
    );
    let program = ApplicationProgram::parse(BCU2_PRODUCT).unwrap();
    let device = DeviceData::parse(&device).unwrap();
    let addresses = GroupAddresses::parse_list(&list).unwrap();
    let result = Download::build(&program, &device, &addresses, DownloadKind::FULL);
    assert!(matches!(
        result,
        Err(knxide_program::Error::TableOverflow(_))
    ));
}

// ------------------------------------------------------------------------------------------
// BIM M112 (mask 0701): fixed table places, memory-mapped load control, read before write.
// ------------------------------------------------------------------------------------------

/// The 20 bytes segment 4400 should hold: object table (2 objects), a 16-bit number, a text, a
/// signed byte, three product bytes no parameter covers.
const M112_SEGMENT: [u8; 20] = [
    0x02, 0x01, 0x00, 0x01, 0x10, 0x6D, 0x07, 0x01, 0x11, 0x53, 0x00, 0x12, 0x34, b'H', b'i', 0x00,
    0xFE, 0xA1, 0xA2, 0xA3,
];

#[test]
fn m112_image_and_tables() {
    let download = build(M112_PRODUCT, M112_DEVICE, "GA-7 4660\n");
    assert_eq!(download.kind, DownloadKind::FULL);
    let tables = download.tables.as_ref().unwrap();
    assert_eq!(tables.placement, None);
    assert_eq!(tables.association_table, 0x4010);
    assert_eq!(image_bytes(&download, 0x4000, 1), [0x02]);
    assert_eq!(image_bytes(&download, 0x4003, 2), [0x12, 0x34]);
    assert_eq!(sent(&download, 0x4000, 9), [0x4000, 0x4003, 0x4004]);
    // Real associations only.
    assert_eq!(image_bytes(&download, 0x4010, 3), [0x01, 0x01, 0x00]);
    assert_eq!(sent(&download, 0x4010, 7), [0x4010, 0x4011, 0x4012]);
    // Object 0 in use: high priority, communication, read, transmit, product bit 0x20 kept.
    // Object 1 not in use: the product byte without the communication bit.
    assert_eq!(image_bytes(&download, 0x4400, 20), M112_SEGMENT);
    assert_eq!(sent(&download, 0x4400, 20).len(), 20);
}

#[test]
fn m112_requests_read_before_write() {
    let download = build(M112_PRODUCT, M112_DEVICE, "GA-7 4660\n");
    let mut device = SimulatedDevice::new(0x0701);
    device.set_property(0, 78, 1, &[0, 0, 0, 0, 0, 0xAB]);
    // The device already holds segment 4400, apart from two bytes.
    let mut present = M112_SEGMENT;
    present[5] = 0x00;
    present[17] = 0x00;
    device.set_memory(0x4400, &present);

    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    let expected = [
        "T_CONNECT",
        "DEVICE_DESCRIPTOR_READ",
        "PROP_READ obj=0 pid=56 start=1 count=1",
        "AUTHORIZE_REQUEST FFFFFFFF",
        // This mask offers the serial number and nothing else to check.
        SERIAL,
        // The comparison step reads the six bytes the property holds.
        "PROP_READ obj=0 pid=78 start=1 count=1",
        // Every load event: an eleven-byte record, then one status byte.
        "MEM_WRITE 0104 1400000000000000000000",
        "MEM_READ B6EA 1",
        "MEM_WRITE 0104 2400000000000000000000",
        "MEM_READ B6EB 1",
        "MEM_WRITE 0104 3400000000000000000000",
        "MEM_READ B6EC 1",
        "MEM_WRITE 0104 1100000000000000000000",
        "MEM_READ B6EA 1",
        "MEM_WRITE 0104 13000040000009FF038000",
        "MEM_READ B6EA 1",
        // Read, write the difference, read it back; the individual address is skipped.
        "MEM_READ 4000 1",
        "MEM_WRITE 4000 02",
        "MEM_READ 4000 1",
        "MEM_READ 4003 2",
        "MEM_WRITE 4003 1234",
        "MEM_READ 4003 2",
        "MEM_WRITE 0104 13020040000000FA020101",
        "MEM_READ B6EA 1",
        "MEM_WRITE 0104 1200000000000000000000",
        "MEM_READ B6EA 1",
        "MEM_WRITE 0104 2100000000000000000000",
        "MEM_READ B6EB 1",
        "MEM_WRITE 0104 23000040100007FF038000",
        "MEM_READ B6EB 1",
        // The third byte already matches: only the first two are written.
        "MEM_READ 4010 3",
        "MEM_WRITE 4010 0101",
        "MEM_READ 4010 2",
        "MEM_WRITE 0104 23020040100000FA020101",
        "MEM_READ B6EB 1",
        "MEM_WRITE 0104 2200000000000000000000",
        "MEM_READ B6EB 1",
        "MEM_WRITE 0104 3100000000000000000000",
        "MEM_READ B6EC 1",
        "MEM_WRITE 0104 33000044000014FF038000",
        "MEM_READ B6EC 1",
        // Twelve bytes per read; one differing byte in each piece.
        "MEM_READ 4400 12",
        "MEM_WRITE 4405 6D",
        "MEM_READ 4405 1",
        "MEM_READ 440C 8",
        "MEM_WRITE 4411 A1",
        "MEM_READ 4411 1",
        "MEM_WRITE 0104 33020044000000FA020101",
        "MEM_READ B6EC 1",
        "MEM_WRITE 0104 3200000000000000000000",
        "MEM_READ B6EC 1",
        "RESTART",
        "T_DISCONNECT",
    ];
    assert_eq!(lines(&plan.requests), expected);
    let after: Vec<u8> = (0x4400..0x4414).map(|at| device.memory(at)).collect();
    assert_eq!(after, M112_SEGMENT);
}

#[test]
fn m112_gives_up_reading_first_when_little_matches() {
    // A segment of 60 bytes, all different from the device's zeros: after 36 mismatching bytes
    // (three pieces) the rest is written and read back without reading first.
    let product = mini_product("MV-0701", 60, "", "");
    let download = build(&product, "<DeviceInstance />", "");
    let mut device = SimulatedDevice::new(0x0701);
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    let memory: Vec<String> = plan
        .requests
        .iter()
        .filter_map(|request| match request {
            Request::MemoryRead { address, .. } if *address >= 0x4400 && *address < 0x4500 => {
                Some(format!("MEM_READ {address:04X}"))
            }
            Request::MemoryWrite { address, .. } if *address >= 0x4400 => {
                Some(format!("MEM_WRITE {address:04X}"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        memory,
        [
            "MEM_READ 4400",
            "MEM_WRITE 4400",
            "MEM_READ 4400",
            "MEM_READ 440C",
            "MEM_WRITE 440C",
            "MEM_READ 440C",
            "MEM_READ 4418",
            "MEM_WRITE 4418",
            "MEM_READ 4418",
            // Given up: write, read back.
            "MEM_WRITE 4424",
            "MEM_READ 4424",
            "MEM_WRITE 4430",
            "MEM_READ 4430",
        ]
    );
}

// ------------------------------------------------------------------------------------------
// BIM M112 (mask 0705): dynamic tables, partial download, checksum comparison.
// ------------------------------------------------------------------------------------------

const PARTIAL_REQUESTS: &[&str] = &[
    "T_CONNECT",
    "DEVICE_DESCRIPTOR_READ",
    "PROP_READ obj=0 pid=56 start=1 count=1",
    "AUTHORIZE_REQUEST FFFFFFFF",
    SERIAL,
    // Does the device hold the application? Load state, identity, second application.
    "MEM_READ B6EC 1",
    "PROP_READ obj=3 pid=13 start=1 count=1",
    "MEM_READ B6ED 1",
    MANUFACTURER,
    // Hardware identification; this product has nothing to compare it with.
    "PROP_READ obj=0 pid=78 start=1 count=1",
    // Checksum comparison: one eight-byte element per request.
    "PROP_READ obj=3 pid=27 start=1 count=1",
    "PROP_READ obj=3 pid=27 start=2 count=1",
    // The tables are unloaded and reloaded, the application is not unloaded.
    "PROP_WRITE obj=1 pid=5 start=1 count=1 04000000000000000000",
    "PROP_WRITE obj=2 pid=5 start=1 count=1 04000000000000000000",
    "PROP_WRITE obj=1 pid=5 start=1 count=1 01000000000000000000",
    "PROP_WRITE obj=1 pid=5 start=1 count=1 030040000005F2038000",
    "PROP_READ obj=0 pid=14 start=1 count=1",
    "PROP_READ obj=0 pid=14 start=1 count=1",
    "PROP_WRITE obj=0 pid=14 start=1 count=1 04",
    "PROP_READ obj=0 pid=14 start=1 count=1",
    "MEM_WRITE 4000 02",
    "MEM_WRITE 4003 0801",
    "PROP_WRITE obj=1 pid=5 start=1 count=1 030240000000FA030101",
    "PROP_WRITE obj=1 pid=5 start=1 count=1 02000000000000000000",
    "PROP_WRITE obj=2 pid=5 start=1 count=1 01000000000000000000",
    "PROP_WRITE obj=2 pid=5 start=1 count=1 03004005000FF2038000",
    "MEM_WRITE 4005 010100",
    "PROP_WRITE obj=2 pid=5 start=1 count=1 030240050000FA030101",
    "PROP_WRITE obj=2 pid=5 start=1 count=1 02000000000000000000",
    // Application: no segment events, only the bytes tables and parameters own.
    "PROP_WRITE obj=3 pid=5 start=1 count=1 01000000000000000000",
    "MEM_WRITE 4100 010080008147C0",
    "MEM_WRITE 4108 5502",
    "MEM_WRITE 410B 0A",
    "PROP_WRITE obj=3 pid=5 start=1 count=1 02000000000000000000",
    "PROP_READ obj=3 pid=27 start=1 count=1",
    "PROP_READ obj=3 pid=27 start=2 count=1",
    "RESTART",
    "T_DISCONNECT",
];

fn partial_plan(first_checksum: [u8; 2]) -> Plan {
    let download = build(PARTIAL_PRODUCT, PARTIAL_DEVICE, "GA-5 2049\n");
    assert_eq!(download.kind, DownloadKind::PARTIAL);
    assert!(download.fallback.is_some());
    let mut device = holding(&download);
    let [high, low] = first_checksum;
    device.set_property(3, 27, 1, &[0, 0, 0, 12, 0x00, 0xF2, high, low]);
    device.set_property(3, 27, 2, &[0, 0, 0, 5, 0x00, 0xF2, 0xBE, 0xEF]);
    download.plan(&mut device, &PlanOptions::default()).unwrap()
}

#[test]
fn partial_image() {
    let download = build(PARTIAL_PRODUCT, PARTIAL_DEVICE, "GA-5 2049\n");
    // Object table (count, RAM pointer, one object), a gap, two parameter bytes (one of a
    // hidden parameter), a gap, the byte with the four-bit parameter.
    assert_eq!(
        sent(&download, 0x4100, 12),
        [
            0x4100, 0x4101, 0x4102, 0x4103, 0x4104, 0x4105, 0x4106, 0x4108, 0x4109, 0x410B
        ]
    );
    assert_eq!(
        image_bytes(&download, 0x4100, 12),
        [
            0x01, 0x00, 0x80, 0x00, 0x81, 0x47, 0xC0, 0xEE, 0x55, 0x02, 0x03, 0x0A
        ]
    );
    // The association table follows the address table without an extra byte.
    let tables = download.tables.as_ref().unwrap();
    assert_eq!(tables.association_table, 0x4005);
    assert_eq!(sent(&download, 0x4000, 20).len(), 3 + 3);
}

#[test]
fn partial_requests_with_matching_stamp() {
    let plan = partial_plan([0x12, 0x34]);
    assert_eq!(lines(&plan.requests), PARTIAL_REQUESTS);
    assert_eq!(plan.stamp_matches, Some(true));
}

#[test]
fn partial_requests_with_other_stamp() {
    let plan = partial_plan([0x12, 0x35]);
    // The comparison fails; the plain partial download is planned all the same.
    assert_eq!(plan.stamp_matches, Some(false));
    assert_eq!(lines(&plan.requests), PARTIAL_REQUESTS);
}

#[test]
fn same_product_as_full_download() {
    let program = ApplicationProgram::parse(PARTIAL_PRODUCT).unwrap();
    let device = DeviceData::parse(PARTIAL_DEVICE).unwrap();
    let addresses = GroupAddresses::parse_list("GA-5 2049\n").unwrap();
    let download = Download::build(&program, &device, &addresses, DownloadKind::FULL).unwrap();
    // Everything the product image holds is sent now, except the unused table space.
    assert_eq!(sent(&download, 0x4100, 12).len(), 12);
    assert!(download.fallback.is_none());
    let mut simulated = holding(&download);
    simulated.set_property(3, 27, 1, &[0, 0, 0, 12, 0x00, 0xF2, 0x12, 0x34]);
    simulated.set_property(3, 27, 2, &[0, 0, 0, 5, 0x00, 0xF2, 0xBE, 0xEF]);
    let plan = download
        .plan(&mut simulated, &PlanOptions::default())
        .unwrap();
    let lines = lines(&plan.requests);
    // Before a full download the device is not asked whether it holds the application, and
    // no checksums are compared.
    assert_eq!(plan.stamp_matches, None);
    assert_eq!(plan.kind, DownloadKind::FULL);
    assert_eq!(
        lines[4..8],
        [
            SERIAL,
            MANUFACTURER,
            "PROP_READ obj=0 pid=78 start=1 count=1",
            "PROP_WRITE obj=1 pid=5 start=1 count=1 04000000000000000000"
        ]
    );
    assert!(lines.contains(&"PROP_WRITE obj=3 pid=5 start=1 count=1 04000000000000000000".into()));
    assert!(lines.contains(&"PROP_WRITE obj=3 pid=5 start=1 count=1 030007000010F2020000".into()));
    assert!(lines.contains(&"PROP_WRITE obj=3 pid=5 start=1 count=1 03004100000CF3038000".into()));
    assert!(lines.contains(&"MEM_WRITE 4100 010080008147C0EE5502030A".into()));
    assert!(lines.contains(&"PROP_WRITE obj=3 pid=5 start=1 count=1 030241000000FA030101".into()));
    assert!(lines.contains(&"PROP_WRITE obj=3 pid=5 start=1 count=1 03040000000000000000".into()));
}

// ------------------------------------------------------------------------------------------
// Planner details, on a minimal product.
// ------------------------------------------------------------------------------------------

/// One segment of `size` bytes at 4400 holding 1, 2, 3, …; `steps` are placed before and
/// `tail` after the load of state machine 3.
fn mini_product(mask: &str, size: usize, steps: &str, tail: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = (1..=size).map(|n| n as u8).collect();
    let mut data = String::new();
    for chunk in bytes.chunks(3) {
        let padded = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let word = u32::from_be_bytes([0, padded[0], padded[1], padded[2]]);
        for (at, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if at <= chunk.len() {
                data.push(ALPHABET[(word >> shift & 63) as usize] as char);
            } else {
                data.push('=');
            }
        }
    }
    format!(
        r#"<ApplicationProgram Id="M-00FA_A-0001-01-0000" ApplicationNumber="1"
             ApplicationVersion="1" MaskVersion="{mask}" LoadProcedureStyle="ProductProcedure">
           <Static>
             <Code><AbsoluteSegment Id="M-00FA_A-0001-01-0000_AS-4400" Address="17408" Size="{size}">
               <Data>{data}</Data></AbsoluteSegment></Code>
             <LoadProcedures><LoadProcedure>
               {steps}
               <LdCtrlLoad LsmIdx="3" />
               <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="17408" Size="{size}" Access="255" MemType="3" SegFlags="128" />
               <LdCtrlLoadCompleted LsmIdx="3" />
               {tail}
             </LoadProcedure></LoadProcedures>
           </Static>
         </ApplicationProgram>"#
    )
}

fn mini(mask: &str, steps: &str, tail: &str) -> Download {
    build(
        &mini_product(mask, 3, steps, tail),
        "<DeviceInstance />",
        "",
    )
}

/// A modelled device that holds `download` and answers `requests` with `responses` first.
fn scripted(download: &Download, requests: &[&str], responses: &[&str]) -> SimulatedDevice {
    let requests: Vec<Request> = requests.iter().map(|l| l.parse().unwrap()).collect();
    let responses: Vec<Response> = responses.iter().map(|l| l.parse().unwrap()).collect();
    let mut device = holding(download);
    device.replay(&requests, &responses).unwrap();
    device
}

/// Plan `download` against a modelled device that holds it.
fn plan(download: &Download) -> Result<Plan, PlanFailure> {
    download.plan(&mut holding(download), &PlanOptions::default())
}

#[test]
fn missing_connect_and_disconnect_are_added() {
    let plan = plan(&mini("MV-0705", "", "")).unwrap();
    let lines = lines(&plan.requests);
    assert_eq!(lines[0], "T_CONNECT");
    assert_eq!(lines.last().unwrap(), "T_DISCONNECT");
    assert!(lines.contains(&"MEM_WRITE 4400 010203".to_string()));
}

#[test]
fn wrong_mask_stops_the_plan() {
    let failure = mini("MV-0705", "", "")
        .plan(&mut SimulatedDevice::new(0x0701), &PlanOptions::default())
        .unwrap_err();
    assert_eq!(
        failure.error,
        PlanError::WrongMask {
            expected: 0x0705,
            actual: 0x0701
        }
    );
    assert_eq!(
        lines(&failure.requests),
        ["T_CONNECT", "DEVICE_DESCRIPTOR_READ"]
    );
}

#[test]
fn insufficient_access_stops_the_plan() {
    let download = mini("MV-0705", "", "");
    let mut device = scripted(
        &download,
        &["AUTHORIZE_REQUEST FFFFFFFF"],
        &["AUTHORIZE_RESPONSE level=3"],
    );
    let failure = download
        .plan(&mut device, &PlanOptions::default())
        .unwrap_err();
    assert_eq!(failure.error, PlanError::AccessDenied { level: 3 });
    assert_eq!(failure.requests.len(), 4);
    assert!(failure.to_string().contains("after 4 requests"));
}

#[test]
fn another_key_is_presented() {
    let options = PlanOptions {
        key: 0x1234_5678,
        ..PlanOptions::default()
    };
    let download = mini("MV-0705", "", "");
    let plan = download.plan(&mut holding(&download), &options).unwrap();
    let lines = lines(&plan.requests);
    assert_eq!(lines[3], "AUTHORIZE_REQUEST 12345678");
    assert_eq!(lines[4], SERIAL);
}

// ------------------------------------------------------------------------------------------
// The pre-check.
// ------------------------------------------------------------------------------------------

const HARDWARE_0705: &str = "PROP_READ obj=0 pid=78 start=1 count=1";

#[test]
fn precheck_stops_before_the_procedure() {
    let download = mini("MV-0705", "", "");
    let found = download
        .precheck(&mut holding(&download), &PlanOptions::default())
        .unwrap();
    assert_eq!(
        lines(&found.requests),
        [
            "T_CONNECT",
            "DEVICE_DESCRIPTOR_READ",
            "PROP_READ obj=0 pid=56 start=1 count=1",
            "AUTHORIZE_REQUEST FFFFFFFF",
            SERIAL,
            MANUFACTURER,
            HARDWARE_0705,
        ]
    );
    assert_eq!(found.kind, DownloadKind::FULL);
    assert_eq!(found.serial_number, Some(vec![0; 6]));
    assert!(found.notes.is_empty());
    // The plan starts with the same requests.
    let plan = plan(&download).unwrap();
    assert_eq!(plan.requests[..7], found.requests[..]);
}

#[test]
fn a_missing_serial_number_is_tolerated() {
    let download = mini("MV-0705", "", "");
    let mut device = scripted(
        &download,
        &[SERIAL],
        &["PROP_RESPONSE obj=0 pid=11 start=1 count=0"],
    );
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    assert_eq!(plan.serial_number, None);
    assert_eq!(plan.notes.len(), 1);
}

#[test]
fn another_manufacturer_stops_the_run() {
    let download = mini("MV-0705", "", "");
    let mut device = holding(&download);
    device.set_property(0, 12, 1, &[0x00, 0xFB]);
    let failure = download
        .plan(&mut device, &PlanOptions::default())
        .unwrap_err();
    assert_eq!(
        failure.error,
        PlanError::WrongManufacturer {
            expected: 0x00FA,
            actual: 0x00FB
        }
    );
    assert!(
        failure
            .error
            .to_string()
            .starts_with("manufacturer check failed")
    );
    // Nothing was written: the manufacturer read is the last request.
    assert_eq!(failure.requests.last().unwrap().to_string(), MANUFACTURER);

    // A device that does not tell its manufacturer stops the run as well.
    let mut silent = scripted(
        &download,
        &[MANUFACTURER],
        &["PROP_RESPONSE obj=0 pid=12 start=1 count=0"],
    );
    let failure = download
        .plan(&mut silent, &PlanOptions::default())
        .unwrap_err();
    assert!(matches!(failure.error, PlanError::ResourceNotFound(_)));

    // Mask 0701 has no such resource: nothing is read, nothing can fail.
    let download = mini("MV-0701", "", "");
    let plan = plan(&download).unwrap();
    assert!(!lines(&plan.requests).contains(&MANUFACTURER.to_string()));
}

/// A product whose hardware identification is a parameter located in property 78 of the
/// device object, as on mask 0705.
fn product_with_hardware_parameter(dynamic: &str) -> String {
    mini_product("MV-0705", 3, "", "").replace(
        "<LoadProcedures>",
        r#"<ParameterTypes>
             <ParameterType Id="M-00FA_A-0001-01-0000_PT-Hardware">
               <TypeRestriction Base="BinaryValue" SizeInBit="48">
                 <Enumeration Id="M-00FA_A-0001-01-0000_PT-Hardware_EN-1" BinaryValue="AAECAwQF" Text="a" Value="1" />
                 <Enumeration Id="M-00FA_A-0001-01-0000_PT-Hardware_EN-2" BinaryValue="CgsMDQ4P" Text="b" Value="2" />
               </TypeRestriction>
             </ParameterType>
           </ParameterTypes>
           <Parameters>
             <Parameter Id="M-00FA_A-0001-01-0000_P-1" Name="Hardware" ParameterType="M-00FA_A-0001-01-0000_PT-Hardware" Text="" Value="2">
               <Property ObjectIndex="0" PropertyId="78" Offset="0" BitOffset="0" />
             </Parameter>
           </Parameters>
           <ParameterRefs>
             <ParameterRef Id="M-00FA_A-0001-01-0000_P-1_R-1" RefId="M-00FA_A-0001-01-0000_P-1" />
           </ParameterRefs>
           <LoadProcedures>"#,
    )
    .replace(
        "</Static>",
        &format!("</Static><Dynamic>{dynamic}</Dynamic>"),
    )
}

#[test]
fn hardware_identification_in_a_property() {
    let in_use = r#"<ParameterRefRef RefId="M-00FA_A-0001-01-0000_P-1_R-1" />"#;
    let download = build(
        &product_with_hardware_parameter(in_use),
        "<DeviceInstance />",
        "",
    );
    let expected = download.hardware.clone().unwrap();
    assert_eq!(expected.data, [0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F]);
    assert_eq!(expected.mask, [0xFF; 6]);
    // The model reports what the image expects.
    assert!(plan(&download).is_ok());

    // Other hardware: the run stops before anything is written.
    let mut other = holding(&download);
    other.set_property(0, 78, 1, &[0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x10]);
    let failure = download
        .plan(&mut other, &PlanOptions::default())
        .unwrap_err();
    assert_eq!(
        failure.error,
        PlanError::WrongHardware {
            device: vec![0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x10],
            image: vec![0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F],
        }
    );
    assert_eq!(
        failure.error.to_string(),
        "hardware check failed: the device reports 0A0B0C0D0E10, the application is for \
         0A0B0C0D0E0F"
    );
    assert_eq!(failure.requests.last().unwrap().to_string(), HARDWARE_0705);

    // A device without the property is not compared.
    let mut without = scripted(
        &download,
        &[HARDWARE_0705],
        &["PROP_RESPONSE obj=0 pid=78 start=1 count=0"],
    );
    assert!(download.plan(&mut without, &PlanOptions::default()).is_ok());

    // The project picks the other member.
    let device = r#"<DeviceInstance><ParameterInstanceRefs>
        <ParameterInstanceRef RefId="P-1_R-1" Value="1" />
      </ParameterInstanceRefs></DeviceInstance>"#;
    let download = build(&product_with_hardware_parameter(in_use), device, "");
    assert_eq!(download.hardware.unwrap().data, [0, 1, 2, 3, 4, 5]);

    // A parameter that is not in use leaves zeros that still count.
    let download = build(
        &product_with_hardware_parameter(""),
        "<DeviceInstance />",
        "",
    );
    let expected = download.hardware.unwrap();
    assert_eq!((expected.data, expected.mask), (vec![0; 6], vec![0xFF; 6]));

    // Without such a parameter the read is made and nothing is compared.
    let download = mini("MV-0705", "", "");
    assert_eq!(download.hardware, None);
    let mut any = holding(&download);
    any.set_property(0, 78, 1, &[9; 6]);
    assert!(download.plan(&mut any, &PlanOptions::default()).is_ok());
}

#[test]
fn hardware_identification_in_memory() {
    // BCU 2 compares one byte of the device with the image byte at 010C, under the mask of
    // the product segment. The made-up product masks that byte out: anything passes.
    let download = build(BCU2_PRODUCT, BCU2_DEVICE, BCU2_ADDRESSES);
    let expected = download.hardware.clone().unwrap();
    assert_eq!((expected.data, expected.mask), (vec![0x00], vec![0x00]));
    let mut device = holding(&download);
    device.set_property(0, 17, 1, &[0x55]);
    assert!(download.plan(&mut device, &PlanOptions::default()).is_ok());

    // With the byte part of the image it has to match.
    let product = BCU2_PRODUCT.replace(
        "<Mask>/wAAAAAAAAAAAAAAAAD///8AAAAAAA==</Mask>",
        "<Mask>/wAAAAAAAAAAAAAA/wD///8AAAAAAA==</Mask>",
    );
    let download = build(&product, BCU2_DEVICE, BCU2_ADDRESSES);
    assert_eq!(download.hardware.clone().unwrap().mask, [0xFF]);
    let mut device = holding(&download);
    assert!(
        download
            .plan(&mut device.clone(), &PlanOptions::default())
            .is_ok()
    );
    device.set_property(0, 17, 1, &[0x55]);
    let failure = download
        .plan(&mut device, &PlanOptions::default())
        .unwrap_err();
    assert_eq!(
        failure.error,
        PlanError::WrongHardware {
            device: vec![0x55],
            image: vec![0x00],
        }
    );
}

/// The partial product on a device in the given state: (load state, application identity,
/// load state of the second application).
fn partial_on(state: u8, identity: [u8; 5], second: u8) -> (Download, SimulatedDevice) {
    let download = build(PARTIAL_PRODUCT, PARTIAL_DEVICE, "GA-5 2049\n");
    let mut device = scripted(
        &download,
        &["MEM_READ B6EC 1", "MEM_READ B6ED 1"],
        &[
            &format!("MEM_RESPONSE B6EC {state:02X}"),
            &format!("MEM_RESPONSE B6ED {second:02X}"),
        ],
    );
    device.set_property(3, 13, 1, &identity);
    (download, device)
}

const PARTIAL_IDENTITY: [u8; 5] = [0x00, 0xFA, 0x03, 0x01, 0x01];

#[test]
fn partial_download_needs_a_loaded_device() {
    // Loaded, same application, no second application: partial.
    let (download, mut device) = partial_on(1, PARTIAL_IDENTITY, 0);
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    assert_eq!(plan.kind, DownloadKind::PARTIAL);
    assert_eq!(plan.full_because, None);
    assert_eq!(plan.device_application, Some(download.application));

    // Not loaded: the identity is not asked for any more, the download becomes a full one and
    // no checksums are compared.
    let (download, mut device) = partial_on(0, PARTIAL_IDENTITY, 0);
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    assert_eq!(plan.kind, DownloadKind::FULL);
    assert!(plan.full_because.unwrap().contains("not loaded"));
    assert_eq!(
        lines(&plan.requests)[4..9],
        [
            SERIAL,
            "MEM_READ B6EC 1",
            MANUFACTURER,
            HARDWARE_0705,
            "PROP_WRITE obj=1 pid=5 start=1 count=1 04000000000000000000",
        ]
    );
    assert_eq!(plan.stamp_matches, None);
    // The full procedure unloads the application, the partial one does not.
    assert!(
        lines(&plan.requests)
            .contains(&"PROP_WRITE obj=3 pid=5 start=1 count=1 04000000000000000000".into())
    );

    // Another version of the application: full, and the second application is not looked at.
    let (download, mut device) = partial_on(1, [0x00, 0xFA, 0x03, 0x01, 0x02], 0);
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    assert_eq!(plan.kind, DownloadKind::FULL);
    assert!(plan.full_because.unwrap().contains("another application"));
    assert_eq!(
        lines(&plan.requests)[5..8],
        [
            "MEM_READ B6EC 1",
            "PROP_READ obj=3 pid=13 start=1 count=1",
            MANUFACTURER
        ]
    );

    // A second application that is loaded is noted and left alone.
    let (download, mut device) = partial_on(1, PARTIAL_IDENTITY, 1);
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    assert_eq!(plan.kind, DownloadKind::PARTIAL);
    assert_eq!(plan.notes.len(), 1);

    // One in another state is in the way, unless its identity is empty.
    let (download, mut device) = partial_on(1, PARTIAL_IDENTITY, 2);
    device.set_property(4, 13, 1, &[0, 1, 0, 0, 0]);
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    assert_eq!(plan.kind, DownloadKind::FULL);
    let (download, mut device) = partial_on(1, PARTIAL_IDENTITY, 2);
    device.set_property(4, 13, 1, &[0; 5]);
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    assert_eq!(plan.kind, DownloadKind::PARTIAL);
}

#[test]
fn an_explicit_partial_download_stops_instead_of_turning_full() {
    let program = ApplicationProgram::parse(PARTIAL_PRODUCT).unwrap();
    let device = DeviceData::parse(PARTIAL_DEVICE).unwrap();
    let addresses = GroupAddresses::parse_list("GA-5 2049\n").unwrap();
    let download = Download::build(&program, &device, &addresses, DownloadKind::PARTIAL).unwrap();
    assert!(download.fallback.is_none());
    let mut unloaded = scripted(&download, &["MEM_READ B6EC 1"], &["MEM_RESPONSE B6EC 00"]);
    let failure = download
        .plan(&mut unloaded, &PlanOptions::default())
        .unwrap_err();
    assert!(matches!(failure.error, PlanError::PartialNotPossible(_)));
    assert_eq!(
        failure.requests.last().unwrap().to_string(),
        "MEM_READ B6EC 1"
    );
}

#[test]
fn the_project_flag_does_not_decide_the_kind() {
    // The project says "not loaded"; the product prefers partial; the device decides.
    let device = PARTIAL_DEVICE.replace("ApplicationProgramLoaded=\"true\"", "");
    let download = build(PARTIAL_PRODUCT, &device, "GA-5 2049\n");
    assert_eq!(download.kind, DownloadKind::PARTIAL);
    assert_eq!(plan(&download).unwrap().kind, DownloadKind::PARTIAL);
    // A product that does not prefer partial gets a full download and no questions.
    let download = build(BCU2_PRODUCT, BCU2_DEVICE, BCU2_ADDRESSES);
    assert_eq!(download.kind, DownloadKind::FULL);
    assert!(download.fallback.is_none());
}

#[test]
fn partial_check_on_bcu2_reads_the_management_style() {
    let program = ApplicationProgram::parse(BCU2_PRODUCT).unwrap();
    let device = DeviceData::parse(BCU2_DEVICE).unwrap();
    let addresses = GroupAddresses::parse_list(BCU2_ADDRESSES).unwrap();
    let download = Download::build(&program, &device, &addresses, DownloadKind::PARTIAL).unwrap();
    let found = download
        .precheck(&mut holding(&download), &PlanOptions::default())
        .unwrap();
    assert_eq!(
        lines(&found.requests)[4..],
        [
            SERIAL,
            // Managed through load state machines: the load state can be asked for.
            "MEM_READ 0115 1",
            "PROP_READ obj=3 pid=5 start=1 count=1",
            "PROP_READ obj=3 pid=13 start=1 count=1",
            MANUFACTURER,
            "PROP_READ obj=0 pid=17 start=1 count=1",
        ]
    );
    // Managed the older way: no load state to ask for.
    let mut direct = holding(&download);
    direct.set_memory(0x0115, &[1]);
    let found = download
        .precheck(&mut direct, &PlanOptions::default())
        .unwrap();
    assert_eq!(
        lines(&found.requests)[5..7],
        ["MEM_READ 0115 1", "PROP_READ obj=3 pid=13 start=1 count=1"]
    );
}

#[test]
fn requiring_the_same_application() {
    let options = PlanOptions {
        require_same_application: true,
        ..PlanOptions::default()
    };
    // A full download does not ask for the identity by itself; the option adds the read at
    // the end of the pre-check.
    let download = mini("MV-0705", "", "");
    let found = download
        .precheck(&mut holding(&download), &options)
        .unwrap();
    assert_eq!(
        lines(&found.requests)[4..],
        [
            SERIAL,
            MANUFACTURER,
            HARDWARE_0705,
            "PROP_READ obj=3 pid=13 start=1 count=1"
        ]
    );
    assert_eq!(found.device_application, Some(download.application));

    let mut other = holding(&download);
    other.set_property(3, 13, 1, &[0x00, 0xFA, 0x00, 0x01, 0x02]);
    let failure = download.plan(&mut other, &options).unwrap_err();
    assert!(matches!(failure.error, PlanError::WrongApplication { .. }));
    assert_eq!(
        failure.error.to_string(),
        "application check failed: the device holds 00FA/0001 version 02, the download is \
         for 00FA/0001 version 01"
    );
    assert_eq!(failure.requests.len(), 8);

    // A device that does not tell passes.
    let mut silent = scripted(
        &download,
        &["PROP_READ obj=3 pid=13 start=1 count=1"],
        &["PROP_RESPONSE obj=3 pid=13 start=1 count=0"],
    );
    assert!(download.plan(&mut silent, &options).is_ok());

    // Where the partial check has read the identity it is not read again, and another
    // application stops the run instead of turning the download into a full one.
    let (download, mut device) = partial_on(1, PARTIAL_IDENTITY, 0);
    let plan = download.plan(&mut device, &options).unwrap();
    let reads = lines(&plan.requests)
        .iter()
        .filter(|line| line.contains("obj=3 pid=13"))
        .count();
    assert_eq!(reads, 1);
    let (download, mut device) = partial_on(1, [0x00, 0xFA, 0x03, 0x01, 0x02], 0);
    let failure = download.plan(&mut device, &options).unwrap_err();
    assert!(matches!(failure.error, PlanError::WrongApplication { .. }));
}

// ------------------------------------------------------------------------------------------
// The procedure.
// ------------------------------------------------------------------------------------------

#[test]
fn unexpected_load_state_stops_the_plan() {
    let load = "PROP_WRITE obj=3 pid=5 start=1 count=1 01000000000000000000";
    let download = mini("MV-0705", "", "");
    let mut device = scripted(
        &download,
        &[load],
        &["PROP_RESPONSE obj=3 pid=5 start=1 count=1 03"],
    );
    let failure = download
        .plan(&mut device, &PlanOptions::default())
        .unwrap_err();
    assert_eq!(
        failure.error,
        PlanError::LoadState {
            object: 3,
            expected: 2,
            actual: 3
        }
    );
    assert_eq!(failure.requests.last().unwrap().to_string(), load);
    // How far the run got: the opening load event is the first step that writes, and it is
    // the one that failed after the device had taken it.
    let progress = &failure.progress;
    assert_eq!(progress.steps, download.procedure.len());
    assert_eq!(progress.steps_completed, 1, "the connect");
    assert_eq!(
        progress.failed_step.as_deref(),
        Some("start loading, object 3")
    );
    assert_eq!(progress.writes, 1);
    assert_eq!(progress.last_write.as_ref().unwrap().to_string(), load);
    assert_eq!(
        progress.to_string(),
        format!(
            "stopped in step 2 of {} (start loading, object 3); 1 steps were completed; the \
             device took 1 write, the last one {load}",
            progress.steps
        )
    );

    // A run that stops in the pre-check has written nothing.
    let failure = download
        .plan(&mut SimulatedDevice::new(0x0701), &PlanOptions::default())
        .unwrap_err();
    assert_eq!(
        failure.progress.to_string(),
        "nothing was written to the device"
    );
    assert_eq!(failure.progress.failed_step, None);
}

#[test]
fn transitional_load_state_is_polled() {
    let completed = "PROP_WRITE obj=3 pid=5 start=1 count=1 02000000000000000000";
    let poll = "PROP_READ obj=3 pid=5 start=1 count=1";
    let download = mini("MV-0705", "", "");
    let mut device = scripted(
        &download,
        &[completed, poll, poll],
        &[
            "PROP_RESPONSE obj=3 pid=5 start=1 count=1 05",
            "PROP_RESPONSE obj=3 pid=5 start=1 count=1 05",
            "PROP_RESPONSE obj=3 pid=5 start=1 count=1 01",
        ],
    );
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    let lines = lines(&plan.requests);
    let at = lines.iter().position(|l| l == completed).unwrap();
    assert_eq!(&lines[at + 1..at + 3], [poll, poll]);
    assert_eq!(lines[at + 3], "T_DISCONNECT");

    // With polling disabled the transitional state is an error.
    let mut device = scripted(
        &download,
        &[completed],
        &["PROP_RESPONSE obj=3 pid=5 start=1 count=1 05"],
    );
    let options = PlanOptions {
        max_state_polls: 0,
        ..PlanOptions::default()
    };
    let failure = download.plan(&mut device, &options).unwrap_err();
    assert!(matches!(
        failure.error,
        PlanError::LoadState { actual: 5, .. }
    ));
}

#[test]
fn verify_mode_already_on_needs_one_read() {
    let download = mini("MV-0705", "", "");
    let mut device = holding(&download);
    device.set_property(0, 14, 1, &[0x04]);
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    let control: Vec<String> = lines(&plan.requests)
        .into_iter()
        .filter(|line| line.contains("pid=14"))
        .collect();
    assert_eq!(control, ["PROP_READ obj=0 pid=14 start=1 count=1"]);
}

#[test]
fn without_device_control_writes_are_read_back() {
    let control = "PROP_READ obj=0 pid=14 start=1 count=1";
    let download = mini("MV-0705", "", "");
    let mut device = scripted(
        &download,
        &[control],
        &["PROP_RESPONSE obj=0 pid=14 start=1 count=0"],
    );
    let plan = download.plan(&mut device, &PlanOptions::default()).unwrap();
    let lines = lines(&plan.requests);
    let at = lines.iter().position(|l| l == control).unwrap();
    assert_eq!(
        &lines[at + 1..at + 3],
        ["MEM_WRITE 4400 010203", "MEM_READ 4400 3"]
    );
}

#[test]
fn control_variables_change_the_write_mode() {
    // Echo mode refused by the procedure: the device is read first, as on a mask without it.
    let steps = r#"<LdCtrlSetControlVariable Name="EnableMemoryAutoVerify" Value="false" />"#;
    let plan_of = |steps: &str| plan(&mini("MV-0705", steps, "")).unwrap();
    let memory: Vec<String> = lines(&plan_of(steps).requests)
        .into_iter()
        .filter(|line| line.starts_with("MEM_"))
        .collect();
    assert_eq!(
        memory,
        [
            "MEM_READ 4400 3",
            "MEM_WRITE 4400 010203",
            "MEM_READ 4400 3"
        ]
    );

    // Reading first refused as well: write and read back.
    let steps = r#"<LdCtrlSetControlVariable Name="EnableMemoryAutoVerify" Value="false" />
                   <LdCtrlSetControlVariable Name="EnableOptimisticWrite" Value="false" />"#;
    let memory: Vec<String> = lines(&plan_of(steps).requests)
        .into_iter()
        .filter(|line| line.starts_with("MEM_"))
        .collect();
    assert_eq!(memory, ["MEM_WRITE 4400 010203", "MEM_READ 4400 3"]);

    // Segment writes switched off: the segment is allocated, nothing is written.
    let steps = r#"<LdCtrlSetControlVariable Name="EnableSegmentWrite" Value="false" />"#;
    assert!(
        !lines(&plan_of(steps).requests)
            .iter()
            .any(|l| l.starts_with("MEM_"))
    );
}

#[test]
fn compare_mismatch_stops_unless_ignored() {
    let compare = r#"<LdCtrlCompareProp ObjIdx="0" PropId="9" InlineData="FA" />"#;
    // A device that passes the pre-check and holds another value than the step expects.
    let mut device = holding(&mini("MV-0705", "", ""));
    device.set_property(0, 9, 1, &[0xFB]);
    let failure = mini("MV-0705", compare, "")
        .plan(&mut device.clone(), &PlanOptions::default())
        .unwrap_err();
    assert!(matches!(failure.error, PlanError::Mismatch(_)));
    assert_eq!(
        failure.requests.last().unwrap().to_string(),
        "PROP_READ obj=0 pid=9 start=1 count=1"
    );
    assert_eq!(failure.requests.len(), 8);

    // A mask that hides the differing bit, an inverted comparison, an ignored mismatch.
    for step in [
        r#"<LdCtrlCompareProp ObjIdx="0" PropId="9" InlineData="FA" Mask="F0" />"#,
        r#"<LdCtrlCompareProp ObjIdx="0" PropId="9" InlineData="FA" Invert="true" />"#,
        r#"<LdCtrlCompareProp ObjIdx="0" PropId="9" InlineData="FA"><OnError Cause="CompareMismatch" Ignore="true" /></LdCtrlCompareProp>"#,
    ] {
        let plan = mini("MV-0705", step, "").plan(&mut device.clone(), &PlanOptions::default());
        assert!(plan.is_ok(), "{step}");
    }

    // A property the device does not have.
    let mut bare = holding(&mini("MV-0705", "", ""));
    let failure = mini("MV-0705", compare, "")
        .plan(&mut bare.clone(), &PlanOptions::default())
        .unwrap_err();
    assert!(matches!(failure.error, PlanError::ResourceNotFound(_)));
    let ignored = r#"<LdCtrlCompareProp ObjIdx="0" PropId="9" InlineData="FA"><OnError Cause="ResourceNotFound" Ignore="1" /></LdCtrlCompareProp>"#;
    assert!(
        mini("MV-0705", ignored, "")
            .plan(&mut bare, &PlanOptions::default())
            .is_ok()
    );
    // The model of a device that holds the download answers what the step expects.
    assert!(plan(&mini("MV-0705", compare, "")).is_ok());
}

#[test]
fn steps_without_rules_stop_the_plan() {
    for step in [
        r#"<LdCtrlRelSegment LsmIdx="4" Size="10" Mode="0" Fill="0" />"#,
        r#"<LdCtrlLoad ObjType="3" Occurrence="0" />"#,
        r#"<LdCtrlWriteProp ObjIdx="0" PropId="200" InlineData="00" />"#,
        r#"<LdCtrlMasterReset EraseCode="1" ChannelNumber="0" />"#,
    ] {
        let failure = plan(&mini("MV-0705", step, "")).unwrap_err();
        assert!(
            matches!(failure.error, PlanError::Unsupported(_)),
            "{step}: {}",
            failure.error
        );
    }
    // Memory-mapped load control only reaches state machines 1 to 4.
    let failure = plan(&mini("MV-0701", r#"<LdCtrlUnload LsmIdx="5" />"#, "")).unwrap_err();
    assert!(matches!(failure.error, PlanError::Unsupported(_)));
}

#[test]
fn announcements_of_the_object_table_are_dropped() {
    // What two recorded products have after their restart: steps on state machine 5 that
    // devices of these masks have no use for. Nothing follows the restart.
    let tail = r#"<LdCtrlRestart /><LdCtrlTaskSegment LsmIdx="5" Address="628" />
                  <LdCtrlLoad LsmIdx="5" /><LdCtrlDisconnect />"#;
    for mask in ["MV-0021", "MV-0705", "MV-0701"] {
        let plan = plan(&mini(mask, "", tail)).unwrap();
        let lines = lines(&plan.requests);
        let at = lines.iter().position(|l| l == "RESTART").unwrap();
        assert_eq!(lines[at..], ["RESTART", "T_DISCONNECT"], "{mask}");
    }
    // On BIM M112, steps for the second application's state machine go as well.
    let steps = r#"<LdCtrlLoad LsmIdx="4" /><LdCtrlLoadCompleted LsmIdx="4" />"#;
    let with = lines(&plan(&mini("MV-0705", steps, "")).unwrap().requests);
    let without = lines(&plan(&mini("MV-0705", "", "")).unwrap().requests);
    assert_eq!(with, without);
    // BCU 2 has no second application: the steps run.
    let with = lines(&plan(&mini("MV-0021", steps, "")).unwrap().requests);
    assert!(with.contains(&"PROP_WRITE obj=4 pid=5 start=1 count=1 01000000000000000000".into()));
}

/// A device that notes the pauses the planner asks for.
struct Pausing {
    inner: SimulatedDevice,
    pauses: Vec<(usize, Duration)>,
    requests: usize,
}

impl Device for Pausing {
    fn exchange(&mut self, request: &Request, expect: Expect) -> Result<Option<Response>, Fault> {
        self.requests += 1;
        self.inner.exchange(request, expect)
    }

    fn pause(&mut self, duration: Duration) -> Result<(), Fault> {
        self.pauses.push((self.requests, duration));
        Ok(())
    }
}

#[test]
fn steps_after_a_restart_reconnect() {
    // A step that survives the filter and needs the device after the restart.
    let tail = r#"<LdCtrlRestart /><LdCtrlLoadCompleted LsmIdx="5" /><LdCtrlDisconnect />"#;
    let download = mini("MV-0705", "", tail);
    let run = |key: u32| {
        let mut device = Pausing {
            inner: holding(&download),
            pauses: Vec::new(),
            requests: 0,
        };
        let options = PlanOptions {
            key,
            ..PlanOptions::default()
        };
        let plan = download.plan(&mut device, &options).unwrap();
        let lines = lines(&plan.requests);
        let at = lines.iter().position(|l| l == "RESTART").unwrap();
        (lines[at..].to_vec(), device.pauses, at)
    };
    // Reconnect: a pause, connect, descriptor. No APDU length read, no key.
    let (after, pauses, restart) = run(0xFFFF_FFFF);
    assert_eq!(
        after,
        [
            "RESTART",
            "T_DISCONNECT",
            "T_CONNECT",
            "DEVICE_DESCRIPTOR_READ",
            "PROP_WRITE obj=5 pid=5 start=1 count=1 02000000000000000000",
            "T_DISCONNECT",
        ]
    );
    // The pause comes after the restart and its disconnect, before the connect.
    assert_eq!(pauses, [(restart + 2, Duration::from_secs(3))]);
    // A key of its own is presented again.
    let (after, _, _) = run(0x1234_5678);
    assert_eq!(
        after[2..5],
        [
            "T_CONNECT",
            "DEVICE_DESCRIPTOR_READ",
            "AUTHORIZE_REQUEST 12345678"
        ]
    );
}

#[test]
fn delay_steps_pause_the_device() {
    let steps = r#"<LdCtrlDelay MilliSeconds="250" />"#;
    let download = mini("MV-0705", "", steps);
    let mut device = Pausing {
        inner: holding(&download),
        pauses: Vec::new(),
        requests: 0,
    };
    download.plan(&mut device, &PlanOptions::default()).unwrap();
    assert_eq!(device.pauses.len(), 1);
    assert_eq!(device.pauses[0].1, Duration::from_millis(250));
}

/// A device that goes out of reach after a number of requests.
struct Failing {
    inner: SimulatedDevice,
    left: usize,
    asked: usize,
}

impl Device for Failing {
    fn exchange(&mut self, request: &Request, expect: Expect) -> Result<Option<Response>, Fault> {
        self.asked += 1;
        if self.left == 0 {
            return Err(Fault("the device closed the connection".into()));
        }
        self.left -= 1;
        self.inner.exchange(request, expect)
    }
}

#[test]
fn a_fault_ends_the_run_at_once() {
    let download = mini("MV-0705", "", "");
    let total = plan(&download).unwrap().requests.len();
    for left in 0..total {
        let mut device = Failing {
            inner: holding(&download),
            left,
            asked: 0,
        };
        let failure = download
            .plan(&mut device, &PlanOptions::default())
            .unwrap_err();
        assert_eq!(
            failure.error,
            PlanError::Bus("the device closed the connection".into()),
            "after {left} requests"
        );
        // Nothing was handed to the device after the request that failed.
        assert_eq!(device.asked, left + 1);
        assert_eq!(failure.requests.len(), left + 1);
    }
}

#[test]
fn memory_writes_expect_an_answer_only_in_verify_mode() {
    struct Noting {
        inner: SimulatedDevice,
        writes: Vec<(String, Expect)>,
    }
    impl Device for Noting {
        fn exchange(
            &mut self,
            request: &Request,
            expect: Expect,
        ) -> Result<Option<Response>, Fault> {
            if matches!(request, Request::MemoryWrite { .. }) {
                self.writes.push((request.to_string(), expect));
            }
            self.inner.exchange(request, expect)
        }
    }
    let run = |mask: &str| {
        let download = mini(mask, "", "");
        let mut device = Noting {
            inner: holding(&download),
            writes: Vec::new(),
        };
        download.plan(&mut device, &PlanOptions::default()).unwrap();
        device.writes
    };
    // Mask 0705 switches verify mode on first: the write is echoed.
    assert_eq!(
        run("MV-0705"),
        [("MEM_WRITE 4400 010203".to_string(), Expect::Answer)]
    );
    // Mask 0701 has no verify mode: neither the load control records nor the data are echoed.
    let writes = run("MV-0701");
    assert!(writes.len() > 1);
    assert!(writes.iter().all(|(_, expect)| *expect == Expect::Nothing));
}

#[test]
fn longer_frames_when_device_and_caller_allow() {
    let product = mini_product("MV-0705", 40, "", "");
    let download = build(&product, "<DeviceInstance />", "");
    let mut device = holding(&download);
    device.set_property(0, 56, 1, &[0x00, 0x37]);
    // The caller's limit stays at 15: nothing changes.
    let plan = download
        .plan(&mut device.clone(), &PlanOptions::default())
        .unwrap();
    assert_eq!(plan.apdu_length, 15);
    // Raised to 55: 52 data bytes per memory frame (unverified beyond the arithmetic).
    let options = PlanOptions {
        max_apdu_length: 55,
        ..PlanOptions::default()
    };
    let plan = download.plan(&mut device, &options).unwrap();
    assert_eq!(plan.apdu_length, 55);
    let writes = lines(&plan.requests)
        .into_iter()
        .filter(|l| l.starts_with("MEM_WRITE"))
        .count();
    assert_eq!(writes, 1);
}

// ------------------------------------------------------------------------------------------
// Refusals and robustness.
// ------------------------------------------------------------------------------------------

#[test]
fn unsupported_products_are_refused() {
    let cases = [
        ("MaskVersion=\"MV-0021\"", "MaskVersion=\"MV-07B0\""),
        (
            "LoadProcedureStyle=\"ProductProcedure\"",
            "LoadProcedureStyle=\"DefaultProcedure\"",
        ),
        (
            "<Options />",
            "<Options ParameterByteOrder=\"LittleEndian\" />",
        ),
        (
            "<Options />",
            "<Options LegacyDoNotReportPropertyWriteErrors=\"1\" />",
        ),
        // A parameter outside its segment.
        (
            "Offset=\"14\" BitOffset=\"0\"",
            "Offset=\"16\" BitOffset=\"0\"",
        ),
        // A default that does not fit its type.
        ("_P-1\" Value=\"6\"", "_P-1\" Value=\"600\""),
        // A reference to a parameter that does not exist.
        (
            "RefId=\"M-00FA_A-0102-03-0000_P-4\"",
            "RefId=\"M-00FA_A-0102-03-0000_P-44\"",
        ),
        // Two segments on the same addresses.
        (
            "Address=\"3000\" Size=\"10\"",
            "Address=\"520\" Size=\"10\"",
        ),
        // A segment beyond 16-bit addresses.
        (
            "Address=\"3000\" Size=\"10\"",
            "Address=\"65530\" Size=\"10\"",
        ),
    ];
    let device = DeviceData::parse(BCU2_DEVICE).unwrap();
    let addresses = GroupAddresses::parse_list(BCU2_ADDRESSES).unwrap();
    for (from, to) in cases {
        assert!(BCU2_PRODUCT.contains(from), "{from}");
        let product = BCU2_PRODUCT.replacen(from, to, 1);
        let program = ApplicationProgram::parse(&product).unwrap();
        let result = Download::build(&program, &device, &addresses, DownloadKind::FULL);
        assert!(result.is_err(), "{to}");
    }
    // Values the project stores are checked like product values.
    let device = DeviceData::parse(&BCU2_DEVICE.replace("Value=\"2\"", "Value=\"x\"")).unwrap();
    let program = ApplicationProgram::parse(BCU2_PRODUCT).unwrap();
    // "x" selects no branch but the default one, which is fine for a selector ...
    assert!(Download::build(&program, &device, &addresses, DownloadKind::FULL).is_ok());
    // ... but not for a number that has to go into memory.
    let device =
        DeviceData::parse(&BCU2_DEVICE.replace("P-9_R-9\" Value=\"2\"", "P-1_R-1\" Value=\"300\""))
            .unwrap();
    assert!(Download::build(&program, &device, &addresses, DownloadKind::FULL).is_err());
    // A project value for a reference the product does not have.
    let device = DeviceData::parse(&BCU2_DEVICE.replace("P-9_R-9", "P-9_R-8")).unwrap();
    assert!(Download::build(&program, &device, &addresses, DownloadKind::FULL).is_err());
}

/// Cutting the input anywhere must give an error or a usable result, never a panic.
#[test]
fn truncated_input_never_panics() {
    let addresses = GroupAddresses::parse_list(BCU2_ADDRESSES).unwrap();
    let device = DeviceData::parse(BCU2_DEVICE).unwrap();
    let mut parsed = 0;
    for (product, step) in [(BCU2_PRODUCT, 7), (M112_PRODUCT, 11), (PARTIAL_PRODUCT, 13)] {
        for end in (0..product.len()).step_by(step) {
            if !product.is_char_boundary(end) {
                continue;
            }
            if let Ok(program) = ApplicationProgram::parse(&product[..end]) {
                parsed += 1;
                let _ = Download::build(&program, &device, &addresses, DownloadKind::FULL);
            }
        }
    }
    assert_eq!(parsed, 0, "a truncated document has unclosed elements");
    for end in 0..BCU2_DEVICE.len() {
        let _ = DeviceData::parse(&BCU2_DEVICE[..end]);
    }
}

/// Damaging single characters must not panic either; many damaged documents still parse.
#[test]
fn damaged_input_never_panics() {
    let addresses = GroupAddresses::parse_list(BCU2_ADDRESSES).unwrap();
    let device = DeviceData::parse(BCU2_DEVICE).unwrap();
    let mut built = 0;
    for (product, mask) in [
        (BCU2_PRODUCT, 0x0021),
        (M112_PRODUCT, 0x0701),
        (PARTIAL_PRODUCT, 0x0705),
    ] {
        let bytes = product.as_bytes();
        for at in (0..bytes.len()).step_by(13) {
            for replacement in *b"9< " {
                let mut damaged = bytes.to_vec();
                damaged[at] = replacement;
                let Ok(text) = String::from_utf8(damaged) else {
                    continue;
                };
                let Ok(program) = ApplicationProgram::parse(&text) else {
                    continue;
                };
                for kind in [DownloadKind::FULL, DownloadKind::PARTIAL] {
                    if let Ok(download) = Download::build(&program, &device, &addresses, kind) {
                        built += 1;
                        let _ =
                            download.plan(&mut SimulatedDevice::new(mask), &PlanOptions::default());
                    }
                }
            }
        }
    }
    println!("{built} damaged documents still gave a download");
    assert!(built > 100, "only {built} damaged documents were usable");
}
