//! Properties that must hold for any input: readers return errors instead of panicking, the
//! text and wire forms of requests and responses round-trip, and the transport connection
//! keeps its books whatever arrives.

use std::time::{Duration, Instant};

use knxide_core::{Apdu, Tpdu};
use knxide_program::config::test_matches;
use knxide_program::transport::{Connection, TransportConfig, TransportEvent};
use knxide_program::{
    ApplicationProgram, DeviceData, DownloadStamp, GroupAddresses, Project, ProjectArchive,
    Request, Response, wire,
};
use proptest::prelude::*;

/// A request as it fits one frame: counts and start indices within their wire fields.
fn wire_request() -> impl Strategy<Value = Request> {
    prop_oneof![
        Just(Request::DeviceDescriptorRead),
        Just(Request::Restart),
        any::<u32>().prop_map(|key| Request::Authorize { key }),
        (any::<u8>(), any::<u8>(), 0u16..0x1000, 0u8..16).prop_map(
            |(object, property, start, count)| Request::PropertyRead {
                object,
                property,
                start,
                count
            }
        ),
        (any::<u8>(), any::<u8>(), 0u16..0x1000, 0u8..16, data()).prop_map(
            |(object, property, start, count, data)| Request::PropertyWrite {
                object,
                property,
                start,
                count,
                data
            }
        ),
        (any::<u16>(), 0u8..64).prop_map(|(address, count)| Request::MemoryRead { address, count }),
        (any::<u16>(), data()).prop_map(|(address, data)| Request::MemoryWrite { address, data }),
    ]
}

fn wire_response() -> impl Strategy<Value = Response> {
    prop_oneof![
        any::<u16>().prop_map(|mask_version| Response::DeviceDescriptor { mask_version }),
        any::<u8>().prop_map(|level| Response::Authorize { level }),
        (
            any::<u8>(),
            any::<u8>(),
            0u16..0x1000,
            0u8..16,
            proptest::collection::vec(any::<u8>(), 0..20)
        )
            .prop_map(
                |(object, property, start, count, data)| Response::Property {
                    object,
                    property,
                    start,
                    count,
                    data
                }
            ),
        (any::<u16>(), proptest::collection::vec(any::<u8>(), 0..20))
            .prop_map(|(address, data)| Response::Memory { address, data }),
    ]
}

fn apdu() -> impl Strategy<Value = Apdu> {
    (0u16..0x400, proptest::collection::vec(any::<u8>(), 0..20))
        .prop_map(|(apci, data)| Apdu::Other { apci, data })
}

fn tpdu() -> impl Strategy<Value = Tpdu> {
    prop_oneof![
        Just(Tpdu::Connect),
        Just(Tpdu::Disconnect),
        (0u8..16).prop_map(|sequence| Tpdu::Ack { sequence }),
        (0u8..16).prop_map(|sequence| Tpdu::Nak { sequence }),
        (0u8..16, apdu()).prop_map(|(sequence, apdu)| Tpdu::Connected { sequence, apdu }),
        apdu().prop_map(Tpdu::Data),
    ]
}

/// What a test can do to a connection.
#[derive(Debug, Clone)]
enum Step {
    Open,
    Close,
    Request(bool),
    Input(Tpdu),
    /// The frame a well-behaved device would send next: the acknowledgement of the pending
    /// frame, or the next in-sequence data frame.
    Cooperate(bool),
    Wait(u16),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        1 => Just(Step::Open),
        1 => Just(Step::Close),
        4 => any::<bool>().prop_map(Step::Request),
        4 => tpdu().prop_map(Step::Input),
        6 => any::<bool>().prop_map(Step::Cooperate),
        3 => (0u16..12_000).prop_map(Step::Wait),
    ]
}

fn data() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 1..20)
}

fn request() -> impl Strategy<Value = Request> {
    prop_oneof![
        Just(Request::Connect),
        Just(Request::Disconnect),
        Just(Request::DeviceDescriptorRead),
        Just(Request::Restart),
        any::<u32>().prop_map(|key| Request::Authorize { key }),
        (any::<u8>(), any::<u8>(), any::<u16>(), any::<u8>()).prop_map(
            |(object, property, start, count)| Request::PropertyRead {
                object,
                property,
                start,
                count
            }
        ),
        (any::<u8>(), any::<u8>(), any::<u16>(), any::<u8>(), data()).prop_map(
            |(object, property, start, count, data)| Request::PropertyWrite {
                object,
                property,
                start,
                count,
                data
            }
        ),
        (any::<u16>(), any::<u8>())
            .prop_map(|(address, count)| Request::MemoryRead { address, count }),
        (any::<u16>(), data()).prop_map(|(address, data)| Request::MemoryWrite { address, data }),
    ]
}

fn response() -> impl Strategy<Value = Response> {
    prop_oneof![
        any::<u16>().prop_map(|mask_version| Response::DeviceDescriptor { mask_version }),
        any::<u8>().prop_map(|level| Response::Authorize { level }),
        (
            any::<u8>(),
            any::<u8>(),
            any::<u16>(),
            any::<u8>(),
            proptest::collection::vec(any::<u8>(), 0..20)
        )
            .prop_map(
                |(object, property, start, count, data)| Response::Property {
                    object,
                    property,
                    start,
                    count,
                    data
                }
            ),
        (any::<u16>(), data()).prop_map(|(address, data)| Response::Memory { address, data }),
    ]
}

proptest! {
    #[test]
    fn requests_round_trip_through_their_apdu(request in wire_request()) {
        let apdu = wire::request_apdu(&request).unwrap();
        // And through the octets of a numbered frame.
        let mut octets = Vec::new();
        Tpdu::Connected { sequence: 5, apdu }.encode(&mut octets).unwrap();
        let Tpdu::Connected { sequence: 5, apdu } = Tpdu::decode(&octets).unwrap() else {
            panic!("not a numbered frame");
        };
        prop_assert_eq!(wire::request_from_apdu(&apdu), Some(request));
    }

    #[test]
    fn responses_round_trip_through_their_apdu(response in wire_response()) {
        let apdu = wire::response_apdu(&response).unwrap();
        let mut octets = Vec::new();
        Tpdu::Connected { sequence: 0, apdu }.encode(&mut octets).unwrap();
        let Tpdu::Connected { apdu, .. } = Tpdu::decode(&octets).unwrap() else {
            panic!("not a numbered frame");
        };
        prop_assert_eq!(wire::response_from_apdu(&apdu), Some(response));
    }

    #[test]
    fn any_frame_is_decoded_without_panic(octets in proptest::collection::vec(any::<u8>(), 0..24)) {
        if let Ok(tpdu) = Tpdu::decode(&octets)
            && let Some(apdu) = tpdu.apdu()
        {
            let _ = wire::response_from_apdu(apdu);
            let _ = wire::request_from_apdu(apdu);
        }
    }

    /// Whatever is done to a connection: it never panics, sequence numbers stay four bits
    /// wide, a timer runs exactly while a request is pending, and every request that was
    /// taken ends in exactly one outcome unless the connection is closed or reopened under it.
    #[test]
    fn connection_keeps_its_books(steps in proptest::collection::vec(step(), 0..80)) {
        let mut now = Instant::now();
        let mut connection = Connection::new(TransportConfig::default());
        let mut pending = false;
        // What a well-behaved device would do next.
        let mut last_sent: Option<u8> = None;
        let mut device_sequence = 0u8;
        for step in steps {
            match step {
                Step::Open => {
                    connection.open();
                    pending = false;
                    device_sequence = 0;
                    last_sent = None;
                }
                Step::Close => {
                    connection.close();
                    pending = false;
                }
                Step::Request(expects_response) => {
                    let taken = connection
                        .request(Apdu::Other { apci: 0x300, data: Vec::new() }, expects_response, now)
                        .is_ok();
                    prop_assert_eq!(taken, connection.is_open() && !pending);
                    pending |= taken;
                }
                Step::Input(tpdu) => {
                    connection.handle_input(&tpdu, now);
                    if let Tpdu::Connected { sequence, .. } = tpdu
                        && sequence == device_sequence
                    {
                        device_sequence = (device_sequence + 1) & 0x0F;
                    }
                }
                Step::Cooperate(acknowledge) => {
                    let tpdu = match (acknowledge, last_sent) {
                        (true, Some(sequence)) => Tpdu::Ack { sequence },
                        _ => {
                            let sequence = device_sequence;
                            device_sequence = (device_sequence + 1) & 0x0F;
                            Tpdu::Connected { sequence, apdu: Apdu::Other { apci: 0x340, data: vec![7, 5] } }
                        }
                    };
                    connection.handle_input(&tpdu, now);
                }
                Step::Wait(milliseconds) => {
                    now += Duration::from_millis(u64::from(milliseconds));
                    connection.handle_timeout(now);
                }
            }
            while let Some(frame) = connection.poll_transmit() {
                match frame {
                    Tpdu::Connected { sequence, .. } => {
                        prop_assert!(sequence < 16);
                        last_sent = Some(sequence);
                    }
                    Tpdu::Ack { sequence } => prop_assert!(sequence < 16),
                    Tpdu::Connect | Tpdu::Disconnect => {}
                    other => prop_assert!(false, "unexpected frame {:?}", other),
                }
            }
            while let Some(event) = connection.poll_event() {
                match event {
                    TransportEvent::Acknowledged
                    | TransportEvent::Response(_)
                    | TransportEvent::Failed(_) => {
                        prop_assert!(pending, "an outcome without a request");
                        pending = false;
                    }
                    TransportEvent::Closed => {
                        prop_assert!(!pending);
                        prop_assert!(!connection.is_open());
                    }
                    TransportEvent::Unsolicited(_) => {}
                }
            }
            prop_assert_eq!(connection.is_busy(), pending);
            prop_assert_eq!(connection.poll_timeout().is_some(), pending);
            if pending {
                prop_assert!(connection.is_open());
            }
        }
        // Left alone, a pending request always comes to an end.
        for _ in 0..6 {
            now += Duration::from_secs(10);
            connection.handle_timeout(now);
        }
        prop_assert!(!connection.is_busy());
    }

    #[test]
    fn archives_survive_any_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
        if let Ok(archive) = ProjectArchive::open(&bytes) {
            let _ = Project::load(&archive);
        }
    }

    #[test]
    fn request_lines_round_trip(request in request()) {
        prop_assert_eq!(request.to_string().parse::<Request>().unwrap(), request);
    }

    #[test]
    fn response_lines_round_trip(response in response()) {
        prop_assert_eq!(response.to_string().parse::<Response>().unwrap(), response);
    }

    #[test]
    fn readers_survive_any_text(text in any::<String>()) {
        let _ = ApplicationProgram::parse(&text);
        let _ = DeviceData::parse(&text);
        let _ = GroupAddresses::parse_list(&text);
        let _ = GroupAddresses::parse_project(&text);
        let _ = DownloadStamp::parse(&text);
        let _ = text.parse::<Request>();
        let _ = text.parse::<Response>();
    }

    /// Text made of the characters XML is made of gets much further into the reader.
    #[test]
    fn readers_survive_markup_soup(
        text in r#"(<|>|/|=|"|'|&|;|!|\?|\[|\]|-| |[A-Za-z0-9#]|<!--|-->|<!\[CDATA\[|\]\]>|<ApplicationProgram |<DeviceInstance |Id="M-0001_A-1"|MaskVersion="MV-0701"){0,60}"#
    ) {
        let _ = ApplicationProgram::parse(&text);
        let _ = DeviceData::parse(&text);
        let _ = GroupAddresses::parse_project(&text);
    }

    #[test]
    fn stamps_survive_any_base64(text in "[A-Za-z0-9+/]{0,80}={0,2}") {
        let _ = DownloadStamp::parse(&text);
    }

    #[test]
    fn test_expressions_survive_any_text(test in any::<String>(), value in any::<String>()) {
        let _ = test_matches(&test, &value);
    }

    #[test]
    fn equal_numbers_match(number in any::<i32>()) {
        let value = number.to_string();
        let at_most = format!("<={number}");
        let other = format!("!={number}");
        prop_assert!(test_matches(&value, &value));
        prop_assert!(test_matches(&at_most, &value));
        prop_assert!(!test_matches(&other, &value));
    }
}
