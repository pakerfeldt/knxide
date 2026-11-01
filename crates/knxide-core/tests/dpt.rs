//! DPT codec fixtures and property tests.

use knxide_core::Payload;
use knxide_core::dpt::*;
use proptest::prelude::*;

fn bytes(b: &[u8]) -> Payload {
    Payload::Bytes(b.to_vec())
}

#[test]
fn dpt9_fixtures() {
    // Vectors from xknx's test suite (MIT), test/dpt_tests/dpt_9_float_test.py.
    for (value, raw) in [
        (-30.0, [0x8A, 0x24]),
        (16.96, [0x06, 0xA0]),
        (0.0, [0x00, 0x00]),
        (21.0, [0x0C, 0x1A]),
        (21.5, [0x0C, 0x33]),
        (500.0, [0x2E, 0x1A]),
        (499.84, [0x2E, 0x1A]),
        (500.16, [0x2E, 0x1B]),
        (-10.0, [0x84, 0x18]),
        (-1000.0, [0xB1, 0xE6]),
        (-999.68, [0xB1, 0xE6]),
        (-1000.32, [0xB1, 0xE5]),
        (dpt9::MAX, [0x7F, 0xFE]),
        (dpt9::MIN, [0xF8, 0x00]),
    ] {
        assert_eq!(dpt9::encode(value), Ok(bytes(&raw)), "{value}");
    }
    assert_eq!(dpt9::decode(&bytes(&[0x87, 0xFF])), Ok(-0.01));
    assert_eq!(dpt9::decode(&bytes(&[0x00, 0x01])), Ok(0.01));
    assert_eq!(
        dpt9::decode(&bytes(&[0x7F, 0xFF])),
        Err(DptError::InvalidData)
    );
    assert_eq!(dpt9::decode(&bytes(&[0x0C])), Err(DptError::PayloadLength));
    assert_eq!(dpt9::encode(670_700.0), Err(DptError::OutOfRange));
    assert_eq!(dpt9::encode(f32::NAN), Err(DptError::OutOfRange));
}

#[test]
fn small_types() {
    assert_eq!(dpt1::encode(true), Payload::Small(1));
    assert_eq!(dpt1::decode(&Payload::Small(0)), Ok(false));
    assert_eq!(dpt1::decode(&Payload::Small(2)), Err(DptError::OutOfRange));
    assert_eq!(dpt1::decode(&bytes(&[1])), Err(DptError::PayloadLength));
    assert_eq!(
        dpt2::encode(Control {
            control: true,
            value: false
        }),
        Payload::Small(0b10)
    );
    let step = Step {
        control: true,
        step_code: 3,
    };
    assert_eq!(dpt3::encode(step), Ok(Payload::Small(0b1011)));
    assert_eq!(dpt3::decode(&Payload::Small(0b1011)), Ok(step));
    assert_eq!(
        dpt3::encode(Step {
            control: false,
            step_code: 8
        }),
        Err(DptError::OutOfRange)
    );
}

#[test]
fn one_byte_types() {
    // DPT 5 is never short-encoded, even for small values.
    assert_eq!(dpt5::encode(1), bytes(&[1]));
    assert_eq!(dpt5::encode_percent(100.0), Ok(bytes(&[0xFF])));
    assert_eq!(dpt5::encode_percent(50.0), Ok(bytes(&[0x80])));
    assert_eq!(dpt5::encode_percent(100.1), Err(DptError::OutOfRange));
    assert_eq!(dpt5::decode_percent(&bytes(&[0xFF])), Ok(100.0));
    assert_eq!(dpt5::encode_angle(360.0), Ok(bytes(&[0xFF])));
    assert_eq!(dpt6::encode(-1), bytes(&[0xFF]));
    assert_eq!(dpt17::encode(1), Ok(bytes(&[0])));
    assert_eq!(dpt17::encode(65), Err(DptError::OutOfRange));
    assert_eq!(dpt17::decode(&bytes(&[63])), Ok(64));
    let learn = SceneControl {
        learn: true,
        scene: 5,
    };
    assert_eq!(dpt18::encode(learn), Ok(bytes(&[0x84])));
    assert_eq!(dpt18::decode(&bytes(&[0x84])), Ok(learn));
    assert_eq!(dpt20_102::encode(HvacMode::Economy), bytes(&[3]));
    assert_eq!(dpt20_102::decode(&bytes(&[5])), Err(DptError::OutOfRange));
}

#[test]
fn multi_byte_types() {
    assert_eq!(dpt7::encode(0x1234), bytes(&[0x12, 0x34]));
    assert_eq!(dpt8::decode(&bytes(&[0xFF, 0xFE])), Ok(-2));
    assert_eq!(dpt12::encode(0xDEAD_BEEF), bytes(&[0xDE, 0xAD, 0xBE, 0xEF]));
    assert_eq!(dpt13::decode(&bytes(&[0xFF; 4])), Ok(-1));
    assert_eq!(dpt14::encode(1.0), Ok(bytes(&[0x3F, 0x80, 0x00, 0x00])));
    // Found by fuzzing: a NaN from the bus must not decode to a value that cannot re-encode.
    assert_eq!(
        dpt14::decode(&bytes(&[0xFF, 0x81, 0x0A, 0x41])),
        Err(DptError::InvalidData)
    );
    let t = TimeOfDay {
        weekday: 1,
        hour: 12,
        minute: 30,
        second: 5,
    };
    assert_eq!(dpt10::encode(t), Ok(bytes(&[0x2C, 30, 5])));
    assert_eq!(dpt10::decode(&bytes(&[0x2C, 30, 5])), Ok(t));
    assert_eq!(
        dpt10::encode(TimeOfDay { hour: 24, ..t }),
        Err(DptError::OutOfRange)
    );
    let d = Date {
        year: 2026,
        month: 11,
        day: 1,
    };
    assert_eq!(dpt11::encode(d), Ok(bytes(&[1, 11, 26])));
    assert_eq!(dpt11::decode(&bytes(&[1, 11, 26])), Ok(d));
    assert_eq!(
        dpt11::decode(&bytes(&[31, 12, 90])).map(|d| d.year),
        Ok(1990)
    );
    assert_eq!(
        dpt11::encode(Date { year: 2090, ..d }),
        Err(DptError::OutOfRange)
    );
    assert_eq!(dpt232::encode(Rgb { r: 1, g: 2, b: 3 }), bytes(&[1, 2, 3]));
}

#[test]
fn strings() {
    let mut raw = b"KNX is OK".to_vec();
    raw.resize(14, 0);
    assert_eq!(dpt16::encode_ascii("KNX is OK"), Ok(bytes(&raw)));
    assert_eq!(
        dpt16::decode_ascii(&bytes(&raw)).as_deref(),
        Ok("KNX is OK")
    );
    assert_eq!(dpt16::encode_ascii("Å"), Err(DptError::OutOfRange));
    assert_eq!(dpt16::encode_latin1("Å").map(|_| ()), Ok(()));
    assert_eq!(
        dpt16::encode_ascii("fifteen chars!!"),
        Err(DptError::OutOfRange)
    );
}

#[test]
fn dpt_ids() {
    for (text, id) in [
        ("9.001", DptId::new(9, 1)),
        ("9.1", DptId::new(9, 1)),
        ("9", DptId::main(9)),
        ("DPST-9-1", DptId::new(9, 1)),
        ("DPT-9", DptId::main(9)),
        ("232.600", DptId::new(232, 600)),
    ] {
        assert_eq!(text.parse(), Ok(id), "{text}");
    }
    assert_eq!(DptId::new(9, 1).to_string(), "9.001");
    assert_eq!(DptId::main(1).to_string(), "1");
    assert!("9.x".parse::<DptId>().is_err());
    assert!(!DptId::new(6, 20).is_supported());
    assert!(!DptId::new(19, 1).is_supported());
}

#[test]
fn dynamic_values() {
    let id = |s: &str| s.parse::<DptId>().unwrap();
    let cases: &[(&str, &str, &str)] = &[
        // (dpt, input text, formatted output)
        ("1.001", "on", "on"),
        ("1.008", "down", "down"),
        ("1", "true", "true"),
        ("2.001", "1,0", "1,0"),
        ("3.007", "+3", "+3"),
        ("3.007", "-0", "-0"),
        ("5.001", "50", "50.2 %"),
        ("5.010", "7", "7"),
        ("6", "-5", "-5"),
        ("9.001", "21.5", "21.5 °C"),
        ("9.004", "-0", "0 lx"),
        ("10.001", "Mon 12:30", "Mon 12:30:00"),
        ("11.001", "2026-11-01", "2026-11-01"),
        ("14.056", "1234.5", "1234.5 W"),
        ("16.000", "hello", "hello"),
        ("17.001", "5", "5"),
        ("18.001", "learn 5", "learn 5"),
        ("20.102", "comfort", "comfort"),
        ("232.600", "#0a0b0c", "#0a0b0c"),
        ("232.600", "1,2,3", "#010203"),
    ];
    for (dpt, input, output) in cases {
        let dpt = id(dpt);
        let value = dpt
            .parse_value(input)
            .unwrap_or_else(|e| panic!("{dpt} {input}: {e}"));
        let payload = dpt
            .encode(&value)
            .unwrap_or_else(|e| panic!("{dpt} {input}: {e}"));
        let decoded = dpt.decode(&payload).unwrap();
        assert_eq!(dpt.format(&decoded).to_string(), *output, "{dpt} {input}");
    }
    // Numeric kinds convert when exact and in range.
    assert_eq!(id("7").encode(&DptValue::Float(3.0)), Ok(bytes(&[0, 3])));
    assert_eq!(
        id("7").encode(&DptValue::Float(3.5)),
        Err(DptError::WrongValueKind)
    );
    assert_eq!(
        id("5").encode(&DptValue::Signed(-1)),
        Err(DptError::OutOfRange)
    );
    assert_eq!(
        id("1").encode(&DptValue::Unsigned(1)),
        Err(DptError::WrongValueKind)
    );
    assert_eq!(id("19.001").parse_value("x"), Err(DptError::Unsupported));
}

proptest! {
    #[test]
    fn dpt9_round_trip_within_quantisation(v in dpt9::MIN..=dpt9::MAX) {
        let decoded = dpt9::decode(&dpt9::encode(v).unwrap()).unwrap();
        // Half a step: 0.005 at E = 0, and |v| / 2048 once the mantissa is at least 1024.
        let tolerance = 0.005 + f64::from(v.abs()) / 2048.0 + 1e-6 * f64::from(v.abs());
        prop_assert!((f64::from(decoded) - f64::from(v)).abs() <= tolerance, "{v} -> {decoded}");
    }

    #[test]
    fn decoders_never_panic(raw in prop::collection::vec(any::<u8>(), 0..16), small in any::<u8>()) {
        for main in [1u16, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 18, 20, 232] {
            for sub in [None, Some(0), Some(1), Some(3), Some(102), Some(600)] {
                let id = DptId { main, sub };
                let _ = id.decode(&Payload::Bytes(raw.clone()));
                let _ = id.decode(&Payload::Small(small));
            }
        }
    }

    #[test]
    fn integers_round_trip(a in any::<i8>(), b in any::<u16>(), c in any::<i16>(), d in any::<u32>(), e in any::<i32>()) {
        prop_assert_eq!(dpt6::decode(&dpt6::encode(a)), Ok(a));
        prop_assert_eq!(dpt7::decode(&dpt7::encode(b)), Ok(b));
        prop_assert_eq!(dpt8::decode(&dpt8::encode(c)), Ok(c));
        prop_assert_eq!(dpt12::decode(&dpt12::encode(d)), Ok(d));
        prop_assert_eq!(dpt13::decode(&dpt13::encode(e)), Ok(e));
    }

    #[test]
    fn dpt14_round_trip(v in any::<f32>().prop_filter("finite", |v| v.is_finite())) {
        prop_assert_eq!(dpt14::decode(&dpt14::encode(v).unwrap()), Ok(v));
    }

    #[test]
    fn time_and_date_round_trip(weekday in 0u8..8, hour in 0u8..24, minute in 0u8..60, second in 0u8..60,
                                year in 1990u16..2090, month in 1u8..13, day in 1u8..32) {
        let t = TimeOfDay { weekday, hour, minute, second };
        prop_assert_eq!(dpt10::decode(&dpt10::encode(t).unwrap()), Ok(t));
        let d = Date { year, month, day };
        prop_assert_eq!(dpt11::decode(&dpt11::encode(d).unwrap()), Ok(d));
    }

    #[test]
    fn strings_round_trip(s in "[ -~]{0,14}") {
        prop_assert_eq!(dpt16::decode_ascii(&dpt16::encode_ascii(&s).unwrap()), Ok(s.clone()));
        prop_assert_eq!(dpt16::decode_latin1(&dpt16::encode_latin1(&s).unwrap()), Ok(s));
    }

    #[test]
    fn percent_round_trip(raw in any::<u8>()) {
        let p = Payload::Bytes(vec![raw]);
        prop_assert_eq!(dpt5::encode_percent(dpt5::decode_percent(&p).unwrap()), Ok(p.clone()));
        prop_assert_eq!(dpt5::encode_angle(dpt5::decode_angle(&p).unwrap()), Ok(p));
    }
}
