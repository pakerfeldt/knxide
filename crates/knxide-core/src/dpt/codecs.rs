//! Typed codecs, one module per datapoint main type. Spec: 3/7/2 (Datapoint Types).
//!
//! Each module has `encode(value) -> Result<Payload, DptError>` and
//! `decode(&Payload) -> Result<value, DptError>`.

use alloc::string::String;
use alloc::vec::Vec;

use super::DptError;
use crate::apdu::{Payload, SMALL_PAYLOAD_MAX};

fn small(payload: &Payload, max: u8) -> Result<u8, DptError> {
    match payload {
        Payload::Small(v) if *v <= max => Ok(*v),
        Payload::Small(_) => Err(DptError::OutOfRange),
        Payload::Bytes(_) => Err(DptError::PayloadLength),
    }
}

fn bytes<const N: usize>(payload: &Payload) -> Result<[u8; N], DptError> {
    match payload {
        Payload::Bytes(b) => b.as_slice().try_into().map_err(|_| DptError::PayloadLength),
        Payload::Small(_) => Err(DptError::PayloadLength),
    }
}

/// Round half to even, as ETS and xknx do (500.0 encodes to `0x2E1A` for DPT 9, not
/// `0x2E1B`). `f64::round_ties_even` needs `std`.
fn round(x: f64) -> f64 {
    let truncated = (x as i64) as f64;
    let diff = x - truncated;
    let away = if x < 0.0 { -1.0 } else { 1.0 };
    let half = 0.5 * away;
    if diff == half {
        if (truncated as i64) % 2 == 0 {
            truncated
        } else {
            truncated + away
        }
    } else if (x < 0.0 && diff < half) || (x >= 0.0 && diff > half) {
        truncated + away
    } else {
        truncated
    }
}

fn finite(v: f32) -> Result<f64, DptError> {
    if v.is_finite() {
        Ok(f64::from(v))
    } else {
        Err(DptError::OutOfRange)
    }
}

/// DPT 1.xxx, 1 bit: boolean (switch, up/down, open/close, …).
pub mod dpt1 {
    use super::*;

    /// Encode a boolean.
    pub fn encode(value: bool) -> Payload {
        Payload::Small(u8::from(value))
    }

    /// Decode a boolean.
    pub fn decode(payload: &Payload) -> Result<bool, DptError> {
        small(payload, 1).map(|v| v == 1)
    }
}

/// DPT 2.xxx, 2 bits: priority control of a boolean.
pub mod dpt2 {
    use super::*;

    /// A controlled boolean.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Control {
        /// `c`: true = priority control active.
        pub control: bool,
        /// `v`: the value, interpreted as for DPT 1.
        pub value: bool,
    }

    /// Encode, `c` in bit 1 and `v` in bit 0.
    pub fn encode(value: Control) -> Payload {
        Payload::Small(u8::from(value.control) << 1 | u8::from(value.value))
    }

    /// Decode.
    pub fn decode(payload: &Payload) -> Result<Control, DptError> {
        let raw = small(payload, 0b11)?;
        Ok(Control {
            control: raw & 0b10 != 0,
            value: raw & 0b01 != 0,
        })
    }
}

/// DPT 3.007 (dimming) and 3.008 (blinds), 4 bits: relative step.
pub mod dpt3 {
    use super::*;

    /// A relative step command.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Step {
        /// `c`: 3.007 true = increase; 3.008 true = down.
        pub control: bool,
        /// Step code 0–7. 0 = break (stop), otherwise the range is divided into 2^(n-1)
        /// intervals.
        pub step_code: u8,
    }

    /// Encode, `c` in bit 3 and the step code in bits 2–0.
    pub fn encode(value: Step) -> Result<Payload, DptError> {
        if value.step_code > 7 {
            return Err(DptError::OutOfRange);
        }
        Ok(Payload::Small(
            u8::from(value.control) << 3 | value.step_code,
        ))
    }

    /// Decode.
    pub fn decode(payload: &Payload) -> Result<Step, DptError> {
        let raw = small(payload, 0x0F)?;
        Ok(Step {
            control: raw & 0x08 != 0,
            step_code: raw & 0x07,
        })
    }
}

/// DPT 5.xxx, 1 byte unsigned. 5.001 and 5.003 are scaled; other subtypes are raw.
pub mod dpt5 {
    use super::*;

    /// Encode a raw byte (5.004, 5.005, 5.006, 5.010).
    pub fn encode(value: u8) -> Payload {
        Payload::Bytes(alloc::vec![value])
    }

    /// Decode a raw byte.
    pub fn decode(payload: &Payload) -> Result<u8, DptError> {
        bytes::<1>(payload).map(|[b]| b)
    }

    fn encode_scaled(value: f32, max: f64) -> Result<Payload, DptError> {
        let v = finite(value)?;
        if !(0.0..=max).contains(&v) {
            return Err(DptError::OutOfRange);
        }
        Ok(encode(round(v * 255.0 / max) as u8))
    }

    /// 5.001 scaling: 0–100 % in 255 steps.
    pub fn encode_percent(value: f32) -> Result<Payload, DptError> {
        encode_scaled(value, 100.0)
    }

    /// 5.001 scaling.
    pub fn decode_percent(payload: &Payload) -> Result<f32, DptError> {
        decode(payload).map(|b| (f64::from(b) * 100.0 / 255.0) as f32)
    }

    /// 5.003 angle: 0–360° in 255 steps.
    pub fn encode_angle(value: f32) -> Result<Payload, DptError> {
        encode_scaled(value, 360.0)
    }

    /// 5.003 angle.
    pub fn decode_angle(payload: &Payload) -> Result<f32, DptError> {
        decode(payload).map(|b| (f64::from(b) * 360.0 / 255.0) as f32)
    }
}

macro_rules! int_dpt {
    ($(#[$doc:meta])* $module:ident, $ty:ty) => {
        $(#[$doc])*
        pub mod $module {
            use super::*;

            /// Encode, big-endian.
            pub fn encode(value: $ty) -> Payload {
                Payload::Bytes(value.to_be_bytes().to_vec())
            }

            /// Decode, big-endian.
            pub fn decode(payload: &Payload) -> Result<$ty, DptError> {
                bytes(payload).map(<$ty>::from_be_bytes)
            }
        }
    };
}

int_dpt!(
    /// DPT 6.xxx, 1 byte signed (percent, counter). 6.020 (status with mode) is not this type.
    dpt6, i8
);
int_dpt!(
    /// DPT 7.xxx, 2 bytes unsigned (pulses, time periods, …).
    dpt7, u16
);
int_dpt!(
    /// DPT 8.xxx, 2 bytes signed.
    dpt8, i16
);
int_dpt!(
    /// DPT 12.xxx, 4 bytes unsigned (counters, volumes).
    dpt12, u32
);
int_dpt!(
    /// DPT 13.xxx, 4 bytes signed (counters, energy).
    dpt13, i32
);

/// DPT 9.xxx, 2-byte float: temperature, lux, humidity, …
///
/// Layout `MEEEEMMM MMMMMMMM`: value = 0.01 × M × 2^E, with M a 12-bit two's-complement
/// mantissa (the top bit is its sign) and E a 4-bit exponent. Spec: 3/7/2 §3.10.
pub mod dpt9 {
    use super::*;

    /// Largest encodable value: 2046 × 2^15 × 0.01 = 670 433.28. M = 2047 at E = 15 would
    /// be `0x7FFF`, the invalid-data marker.
    pub const MAX: f32 = (2046 * 32768) as f32 / 100.0;
    /// Smallest encodable value: −2048 × 2^15 × 0.01 = −671 088.64.
    pub const MIN: f32 = -(2048 * 32768) as f32 / 100.0;
    /// Raw value reserved to signal invalid data.
    pub const INVALID: [u8; 2] = [0x7F, 0xFF];

    /// Encode, using the smallest exponent whose mantissa fits (the most precise encoding).
    pub fn encode(value: f32) -> Result<Payload, DptError> {
        let hundredths = finite(value)? * 100.0;
        for exponent in 0..=15u8 {
            let mantissa = round(hundredths / f64::from(1u32 << exponent));
            if (-2048.0..=2047.0).contains(&mantissa) {
                let m12 = (mantissa as i16 as u16) & 0x0FFF;
                let high = ((m12 >> 4) & 0x80) as u8 | exponent << 3 | ((m12 >> 8) & 0x07) as u8;
                if [high, m12 as u8] == INVALID {
                    break;
                }
                return Ok(Payload::Bytes(alloc::vec![high, m12 as u8]));
            }
        }
        Err(DptError::OutOfRange)
    }

    /// Decode. [`INVALID`] is reported as [`DptError::InvalidData`].
    pub fn decode(payload: &Payload) -> Result<f32, DptError> {
        let raw = bytes::<2>(payload)?;
        if raw == INVALID {
            return Err(DptError::InvalidData);
        }
        let [high, low] = raw;
        let exponent = (high >> 3) & 0x0F;
        let m12 = u16::from(high & 0x80) << 4 | u16::from(high & 0x07) << 8 | u16::from(low);
        // Sign-extend the 12-bit mantissa.
        let mantissa = ((m12 << 4) as i16) >> 4;
        Ok((f64::from(mantissa) * f64::from(1u32 << exponent) * 0.01) as f32)
    }
}

/// DPT 14.xxx, 4-byte IEEE 754 float: power, energy, current, …
pub mod dpt14 {
    use super::*;

    /// Encode. NaN and infinities are rejected.
    pub fn encode(value: f32) -> Result<Payload, DptError> {
        finite(value)?;
        Ok(Payload::Bytes(value.to_be_bytes().to_vec()))
    }

    /// Decode. NaN and infinities are reported as [`DptError::InvalidData`].
    pub fn decode(payload: &Payload) -> Result<f32, DptError> {
        let value = f32::from_be_bytes(bytes(payload)?);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(DptError::InvalidData)
        }
    }
}

/// DPT 10.001, 3 bytes: time of day with optional weekday.
pub mod dpt10 {
    use super::*;

    /// Time of day.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct TimeOfDay {
        /// 0 = no day, 1 = Monday … 7 = Sunday.
        pub weekday: u8,
        /// 0–23.
        pub hour: u8,
        /// 0–59.
        pub minute: u8,
        /// 0–59.
        pub second: u8,
    }

    fn check(t: TimeOfDay) -> Result<TimeOfDay, DptError> {
        if t.weekday > 7 || t.hour > 23 || t.minute > 59 || t.second > 59 {
            return Err(DptError::OutOfRange);
        }
        Ok(t)
    }

    /// Encode: `DDDHHHHH 00MMMMMM 00SSSSSS`.
    pub fn encode(value: TimeOfDay) -> Result<Payload, DptError> {
        let t = check(value)?;
        Ok(Payload::Bytes(alloc::vec![
            t.weekday << 5 | t.hour,
            t.minute,
            t.second
        ]))
    }

    /// Decode. Reserved bits are ignored.
    pub fn decode(payload: &Payload) -> Result<TimeOfDay, DptError> {
        let [a, b, c] = bytes(payload)?;
        check(TimeOfDay {
            weekday: a >> 5,
            hour: a & 0x1F,
            minute: b & 0x3F,
            second: c & 0x3F,
        })
    }
}

/// DPT 11.001, 3 bytes: date, years 1990–2089.
pub mod dpt11 {
    use super::*;

    /// A calendar date. Day-of-month is checked against 1–31 only.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Date {
        /// 1990–2089.
        pub year: u16,
        /// 1–12.
        pub month: u8,
        /// 1–31.
        pub day: u8,
    }

    fn check(d: Date) -> Result<Date, DptError> {
        if !(1990..=2089).contains(&d.year)
            || !(1..=12).contains(&d.month)
            || !(1..=31).contains(&d.day)
        {
            return Err(DptError::OutOfRange);
        }
        Ok(d)
    }

    /// Encode: `000DDDDD 0000MMMM 0YYYYYYY`, year as 0–99 (≥ 90 means 19xx).
    pub fn encode(value: Date) -> Result<Payload, DptError> {
        let d = check(value)?;
        let year = (d.year % 100) as u8;
        Ok(Payload::Bytes(alloc::vec![d.day, d.month, year]))
    }

    /// Decode. Reserved bits are ignored.
    pub fn decode(payload: &Payload) -> Result<Date, DptError> {
        let [a, b, c] = bytes(payload)?;
        let yy = u16::from(c & 0x7F);
        if yy > 99 {
            return Err(DptError::OutOfRange);
        }
        let year = if yy >= 90 { 1900 + yy } else { 2000 + yy };
        check(Date {
            year,
            month: b & 0x0F,
            day: a & 0x1F,
        })
    }
}

/// DPT 16.000 (ASCII) and 16.001 (ISO 8859-1), 14 bytes: text, zero-padded.
pub mod dpt16 {
    use super::*;

    /// Length of the field on the wire.
    pub const LEN: usize = 14;

    /// Encode 16.001: characters up to U+00FF, at most 14 of them.
    pub fn encode_latin1(value: &str) -> Result<Payload, DptError> {
        let mut out = Vec::with_capacity(LEN);
        for ch in value.chars() {
            let byte = u8::try_from(u32::from(ch)).map_err(|_| DptError::OutOfRange)?;
            out.push(byte);
        }
        if out.len() > LEN {
            return Err(DptError::OutOfRange);
        }
        out.resize(LEN, 0);
        Ok(Payload::Bytes(out))
    }

    /// Encode 16.000: ASCII only, at most 14 characters.
    pub fn encode_ascii(value: &str) -> Result<Payload, DptError> {
        if !value.is_ascii() {
            return Err(DptError::OutOfRange);
        }
        encode_latin1(value)
    }

    /// Decode 16.001. Stops at the first NUL.
    pub fn decode_latin1(payload: &Payload) -> Result<String, DptError> {
        let raw = bytes::<LEN>(payload)?;
        Ok(raw
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| char::from(b))
            .collect())
    }

    /// Decode 16.000. Stops at the first NUL; non-ASCII bytes are an error.
    pub fn decode_ascii(payload: &Payload) -> Result<String, DptError> {
        let s = decode_latin1(payload)?;
        if s.is_ascii() {
            Ok(s)
        } else {
            Err(DptError::OutOfRange)
        }
    }
}

/// DPT 17.001, 1 byte: scene number.
///
/// Scenes are numbered 1–64 here, as in ETS; the wire carries 0–63.
pub mod dpt17 {
    use super::*;

    /// Encode a scene number 1–64.
    pub fn encode(scene: u8) -> Result<Payload, DptError> {
        if !(1..=64).contains(&scene) {
            return Err(DptError::OutOfRange);
        }
        Ok(Payload::Bytes(alloc::vec![scene - 1]))
    }

    /// Decode to a scene number 1–64. Reserved bits are ignored.
    pub fn decode(payload: &Payload) -> Result<u8, DptError> {
        bytes::<1>(payload).map(|[b]| (b & 0x3F) + 1)
    }
}

/// DPT 18.001, 1 byte: scene control (activate or learn).
pub mod dpt18 {
    use super::*;

    /// Activate or learn a scene. Scenes are numbered 1–64, as in ETS.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SceneControl {
        /// true = store the current state as the scene, false = activate it.
        pub learn: bool,
        /// Scene number 1–64.
        pub scene: u8,
    }

    /// Encode: `C0SSSSSS`.
    pub fn encode(value: SceneControl) -> Result<Payload, DptError> {
        if !(1..=64).contains(&value.scene) {
            return Err(DptError::OutOfRange);
        }
        Ok(Payload::Bytes(alloc::vec![
            u8::from(value.learn) << 7 | (value.scene - 1)
        ]))
    }

    /// Decode. The reserved bit is ignored.
    pub fn decode(payload: &Payload) -> Result<SceneControl, DptError> {
        let [b] = bytes(payload)?;
        Ok(SceneControl {
            learn: b & 0x80 != 0,
            scene: (b & 0x3F) + 1,
        })
    }
}

/// DPT 20.102, 1 byte: HVAC operating mode.
pub mod dpt20_102 {
    use super::*;

    /// HVAC mode.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum HvacMode {
        /// 0.
        Auto,
        /// 1.
        Comfort,
        /// 2.
        Standby,
        /// 3, also called night.
        Economy,
        /// 4, frost or heat protection.
        BuildingProtection,
    }

    impl HvacMode {
        /// All modes, in wire order.
        pub const ALL: [Self; 5] = [
            Self::Auto,
            Self::Comfort,
            Self::Standby,
            Self::Economy,
            Self::BuildingProtection,
        ];

        /// Lower-case name, as used when parsing and formatting.
        pub const fn name(self) -> &'static str {
            match self {
                Self::Auto => "auto",
                Self::Comfort => "comfort",
                Self::Standby => "standby",
                Self::Economy => "economy",
                Self::BuildingProtection => "building-protection",
            }
        }
    }

    /// Encode.
    pub fn encode(value: HvacMode) -> Payload {
        let raw = HvacMode::ALL.iter().position(|m| *m == value).unwrap_or(0) as u8;
        Payload::Bytes(alloc::vec![raw])
    }

    /// Decode. Values above 4 are reserved and rejected.
    pub fn decode(payload: &Payload) -> Result<HvacMode, DptError> {
        let [b] = bytes(payload)?;
        HvacMode::ALL
            .get(usize::from(b))
            .copied()
            .ok_or(DptError::OutOfRange)
    }
}

/// DPT 232.600, 3 bytes: RGB colour.
pub mod dpt232 {
    use super::*;

    /// An RGB colour.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Rgb {
        /// Red.
        pub r: u8,
        /// Green.
        pub g: u8,
        /// Blue.
        pub b: u8,
    }

    /// Encode.
    pub fn encode(value: Rgb) -> Payload {
        Payload::Bytes(alloc::vec![value.r, value.g, value.b])
    }

    /// Decode.
    pub fn decode(payload: &Payload) -> Result<Rgb, DptError> {
        bytes(payload).map(|[r, g, b]| Rgb { r, g, b })
    }
}

// Guard: every small DPT fits in the APCI octet.
const _: () = assert!(0x0F <= SMALL_PAYLOAD_MAX);
