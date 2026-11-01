//! Datapoint types (DPTs): how group values are encoded on the bus. Spec: 3/7/2.
//!
//! The bus carries no type information, so the application must know each group address's
//! DPT. Use the typed codecs ([`dpt9::encode`], …) when the type is known at compile time, or
//! [`DptId`] with [`DptValue`] when it comes from configuration.
//!
//! ```
//! use knxide_core::dpt::{self, DptId, DptValue};
//!
//! let payload = dpt::dpt9::encode(21.5).unwrap();
//! let id: DptId = "9.001".parse().unwrap();
//! assert_eq!(id.decode(&payload), Ok(DptValue::Float(21.5)));
//! assert_eq!(id.format(&DptValue::Float(21.5)).to_string(), "21.5 °C");
//! ```

mod codecs;

use alloc::string::{String, ToString};
use core::fmt;
use core::str::FromStr;

pub use codecs::*;
pub use dpt2::Control;
pub use dpt3::Step;
pub use dpt10::TimeOfDay;
pub use dpt11::Date;
pub use dpt18::SceneControl;
pub use dpt20_102::HvacMode;
pub use dpt232::Rgb;

use crate::apdu::Payload;

/// A value could not be encoded or decoded as the requested datapoint type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DptError {
    /// The payload has the wrong size or form (small vs. bytes) for this type.
    PayloadLength,
    /// The value is outside the type's range.
    OutOfRange,
    /// The payload is the type's "invalid data" marker (e.g. `0x7FFF` for DPT 9).
    InvalidData,
    /// The [`DptValue`] variant does not fit this type.
    WrongValueKind,
    /// The datapoint type is not supported.
    Unsupported,
    /// Text could not be parsed as a value or a DPT id.
    Parse,
}

impl fmt::Display for DptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PayloadLength => "payload has the wrong length for this datapoint type",
            Self::OutOfRange => "value out of range for this datapoint type",
            Self::InvalidData => "payload marks the value as invalid",
            Self::WrongValueKind => "value kind does not match the datapoint type",
            Self::Unsupported => "unsupported datapoint type",
            Self::Parse => "could not parse value",
        })
    }
}

impl core::error::Error for DptError {}

/// A datapoint type id: main number and optional subtype, e.g. `9.001` or just `9`.
///
/// Parses `9.001`, `9.1`, `9`, and the ETS forms `DPST-9-1` and `DPT-9`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DptId {
    /// Main number.
    pub main: u16,
    /// Subtype, if known.
    pub sub: Option<u16>,
}

impl DptId {
    /// `main.sub`.
    pub const fn new(main: u16, sub: u16) -> Self {
        Self {
            main,
            sub: Some(sub),
        }
    }

    /// Main type only.
    pub const fn main(main: u16) -> Self {
        Self { main, sub: None }
    }
}

impl fmt::Display for DptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.sub {
            Some(sub) => write!(f, "{}.{sub:03}", self.main),
            None => write!(f, "{}", self.main),
        }
    }
}

impl FromStr for DptId {
    type Err = DptError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let num = |p: &str| p.parse::<u16>().map_err(|_| DptError::Parse);
        if let Some(rest) = s.strip_prefix("DPST-") {
            let (main, sub) = rest.split_once('-').ok_or(DptError::Parse)?;
            return Ok(Self::new(num(main)?, num(sub)?));
        }
        if let Some(main) = s.strip_prefix("DPT-") {
            return Ok(Self::main(num(main)?));
        }
        match s.split_once('.') {
            Some((main, sub)) => Ok(Self::new(num(main)?, num(sub)?)),
            None => Ok(Self::main(num(s)?)),
        }
    }
}

/// How a DPT is encoded, resolved from its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Bool,
    Control,
    Step,
    U8,
    Percent,
    Angle,
    I8,
    U16,
    I16,
    Float16,
    TimeOfDay,
    Date,
    U32,
    I32,
    Float32,
    Ascii,
    Latin1,
    Scene,
    SceneControl,
    HvacMode,
    Rgb,
}

impl DptId {
    fn codec(self) -> Result<Codec, DptError> {
        let sub = self.sub;
        Ok(match (self.main, sub) {
            (1, _) => Codec::Bool,
            (2, _) => Codec::Control,
            (3, _) => Codec::Step,
            (5, Some(1)) => Codec::Percent,
            (5, Some(3)) => Codec::Angle,
            (5, _) => Codec::U8,
            (6, Some(20)) => return Err(DptError::Unsupported),
            (6, _) => Codec::I8,
            (7, _) => Codec::U16,
            (8, _) => Codec::I16,
            (9, _) => Codec::Float16,
            (10, None | Some(1)) => Codec::TimeOfDay,
            (11, None | Some(1)) => Codec::Date,
            (12, _) => Codec::U32,
            (13, _) => Codec::I32,
            (14, _) => Codec::Float32,
            (16, None | Some(0)) => Codec::Ascii,
            (16, Some(1)) => Codec::Latin1,
            (17, None | Some(1)) => Codec::Scene,
            (18, None | Some(1)) => Codec::SceneControl,
            (20, Some(102)) => Codec::HvacMode,
            (232, None | Some(600)) => Codec::Rgb,
            _ => return Err(DptError::Unsupported),
        })
    }

    /// What kind of value the type carries, for choosing an input control. `None` if the
    /// type is not supported.
    pub fn value_kind(self) -> Option<ValueKind> {
        let float = |min: f32, max: f32| ValueKind::Float { min, max };
        let int = |min: i64, max: i64| ValueKind::Integer { min, max };
        Some(match self.codec().ok()? {
            Codec::Bool => {
                let (off, on) = self.bool_labels().unwrap_or(("off", "on"));
                ValueKind::Bool { off, on }
            }
            Codec::Control => ValueKind::Control,
            Codec::Step => ValueKind::Step,
            Codec::U8 => int(0, 255),
            Codec::Percent => float(0.0, 100.0),
            Codec::Angle => float(0.0, 360.0),
            Codec::I8 => int(i8::MIN.into(), i8::MAX.into()),
            Codec::U16 => int(0, u16::MAX.into()),
            Codec::I16 => int(i16::MIN.into(), i16::MAX.into()),
            Codec::Float16 => float(dpt9::MIN, dpt9::MAX),
            Codec::TimeOfDay => ValueKind::TimeOfDay,
            Codec::Date => ValueKind::Date,
            Codec::U32 => int(0, u32::MAX.into()),
            Codec::I32 => int(i32::MIN.into(), i32::MAX.into()),
            Codec::Float32 => float(f32::MIN, f32::MAX),
            Codec::Ascii | Codec::Latin1 => ValueKind::Text {
                max_len: dpt16::LEN,
            },
            Codec::Scene => int(1, 64),
            Codec::SceneControl => ValueKind::SceneControl,
            Codec::HvacMode => ValueKind::HvacMode,
            Codec::Rgb => ValueKind::Rgb,
        })
    }

    /// Whether this crate can encode and decode the type.
    pub fn is_supported(self) -> bool {
        self.codec().is_ok()
    }

    /// Decode a payload.
    pub fn decode(self, payload: &Payload) -> Result<DptValue, DptError> {
        Ok(match self.codec()? {
            Codec::Bool => DptValue::Bool(dpt1::decode(payload)?),
            Codec::Control => DptValue::Control(dpt2::decode(payload)?),
            Codec::Step => DptValue::Step(dpt3::decode(payload)?),
            Codec::U8 => DptValue::Unsigned(dpt5::decode(payload)?.into()),
            Codec::Percent => DptValue::Float(dpt5::decode_percent(payload)?),
            Codec::Angle => DptValue::Float(dpt5::decode_angle(payload)?),
            Codec::I8 => DptValue::Signed(dpt6::decode(payload)?.into()),
            Codec::U16 => DptValue::Unsigned(dpt7::decode(payload)?.into()),
            Codec::I16 => DptValue::Signed(dpt8::decode(payload)?.into()),
            Codec::Float16 => DptValue::Float(dpt9::decode(payload)?),
            Codec::TimeOfDay => DptValue::TimeOfDay(dpt10::decode(payload)?),
            Codec::Date => DptValue::Date(dpt11::decode(payload)?),
            Codec::U32 => DptValue::Unsigned(dpt12::decode(payload)?),
            Codec::I32 => DptValue::Signed(dpt13::decode(payload)?),
            Codec::Float32 => DptValue::Float(dpt14::decode(payload)?),
            Codec::Ascii => DptValue::String(dpt16::decode_ascii(payload)?),
            Codec::Latin1 => DptValue::String(dpt16::decode_latin1(payload)?),
            Codec::Scene => DptValue::Unsigned(dpt17::decode(payload)?.into()),
            Codec::SceneControl => DptValue::SceneControl(dpt18::decode(payload)?),
            Codec::HvacMode => DptValue::HvacMode(dpt20_102::decode(payload)?),
            Codec::Rgb => DptValue::Rgb(dpt232::decode(payload)?),
        })
    }

    /// Encode a value. Numeric values are converted between [`DptValue::Unsigned`],
    /// [`DptValue::Signed`] and [`DptValue::Float`] when the result is exact and in range.
    pub fn encode(self, value: &DptValue) -> Result<Payload, DptError> {
        let codec = self.codec()?;
        let int = |min: i64, max: i64| -> Result<i64, DptError> {
            let v = value.as_i64().ok_or(DptError::WrongValueKind)?;
            if (min..=max).contains(&v) {
                Ok(v)
            } else {
                Err(DptError::OutOfRange)
            }
        };
        let float = || value.as_f32().ok_or(DptError::WrongValueKind);
        let wrong = || Err(DptError::WrongValueKind);
        match (codec, value) {
            (Codec::Bool, DptValue::Bool(v)) => Ok(dpt1::encode(*v)),
            (Codec::Control, DptValue::Control(v)) => Ok(dpt2::encode(*v)),
            (Codec::Step, DptValue::Step(v)) => dpt3::encode(*v),
            (Codec::U8, _) => Ok(dpt5::encode(int(0, 0xFF)? as u8)),
            (Codec::Percent, _) => dpt5::encode_percent(float()?),
            (Codec::Angle, _) => dpt5::encode_angle(float()?),
            (Codec::I8, _) => Ok(dpt6::encode(int(i8::MIN.into(), i8::MAX.into())? as i8)),
            (Codec::U16, _) => Ok(dpt7::encode(int(0, u16::MAX.into())? as u16)),
            (Codec::I16, _) => Ok(dpt8::encode(int(i16::MIN.into(), i16::MAX.into())? as i16)),
            (Codec::Float16, _) => dpt9::encode(float()?),
            (Codec::TimeOfDay, DptValue::TimeOfDay(v)) => dpt10::encode(*v),
            (Codec::Date, DptValue::Date(v)) => dpt11::encode(*v),
            (Codec::U32, _) => Ok(dpt12::encode(int(0, u32::MAX.into())? as u32)),
            (Codec::I32, _) => Ok(dpt13::encode(int(i32::MIN.into(), i32::MAX.into())? as i32)),
            (Codec::Float32, _) => dpt14::encode(float()?),
            (Codec::Ascii, DptValue::String(s)) => dpt16::encode_ascii(s),
            (Codec::Latin1, DptValue::String(s)) => dpt16::encode_latin1(s),
            (Codec::Scene, _) => dpt17::encode(int(1, 64)? as u8),
            (Codec::SceneControl, DptValue::SceneControl(v)) => dpt18::encode(*v),
            (Codec::HvacMode, DptValue::HvacMode(v)) => Ok(dpt20_102::encode(*v)),
            (Codec::Rgb, DptValue::Rgb(v)) => Ok(dpt232::encode(*v)),
            _ => wrong(),
        }
    }

    /// Parse a value from text, as typed on a command line.
    ///
    /// | Type | Accepted text |
    /// |---|---|
    /// | 1.xxx | `on`/`off`, `true`/`false`, `1`/`0`, and the subtype's labels (`up`/`down`, …) |
    /// | 2.xxx | `control,value`, each as for DPT 1, e.g. `1,0` |
    /// | 3.xxx | signed step code: `+3` (increase/down), `-3` (decrease/up), `0` = break |
    /// | numbers | decimal, e.g. `21.5`, `-3`, `255` |
    /// | 10.001 | `[Mon ]HH:MM[:SS]` |
    /// | 11.001 | `YYYY-MM-DD` |
    /// | 16.xxx | the text itself |
    /// | 18.001 | `5` (activate scene 5) or `learn 5` |
    /// | 20.102 | `auto`, `comfort`, `standby`, `economy`, `building-protection`, or 0–4 |
    /// | 232.600 | `#RRGGBB` or `r,g,b` |
    pub fn parse_value(self, text: &str) -> Result<DptValue, DptError> {
        let text = text.trim();
        Ok(match self.codec()? {
            Codec::Bool => DptValue::Bool(self.parse_bool(text)?),
            Codec::Control => {
                let (c, v) = text.split_once(',').ok_or(DptError::Parse)?;
                DptValue::Control(Control {
                    control: parse_bool(c.trim(), None)?,
                    value: self.parse_bool(v.trim())?,
                })
            }
            Codec::Step => {
                let n: i8 = text.parse().map_err(|_| DptError::Parse)?;
                let step_code = n.unsigned_abs();
                if step_code > 7 {
                    return Err(DptError::OutOfRange);
                }
                DptValue::Step(Step {
                    control: n > 0 || text.starts_with('+'),
                    step_code,
                })
            }
            Codec::U8 | Codec::U16 | Codec::U32 | Codec::Scene => {
                DptValue::Unsigned(text.parse().map_err(|_| DptError::Parse)?)
            }
            Codec::I8 | Codec::I16 | Codec::I32 => {
                DptValue::Signed(text.parse().map_err(|_| DptError::Parse)?)
            }
            Codec::Percent | Codec::Angle | Codec::Float16 | Codec::Float32 => {
                DptValue::Float(text.parse().map_err(|_| DptError::Parse)?)
            }
            Codec::TimeOfDay => DptValue::TimeOfDay(parse_time(text)?),
            Codec::Date => DptValue::Date(parse_date(text)?),
            Codec::Ascii | Codec::Latin1 => DptValue::String(text.to_string()),
            Codec::SceneControl => {
                let (learn, scene) = match text.strip_prefix("learn") {
                    Some(rest) => (true, rest.trim()),
                    None => (false, text),
                };
                let scene = scene.parse().map_err(|_| DptError::Parse)?;
                DptValue::SceneControl(SceneControl { learn, scene })
            }
            Codec::HvacMode => {
                let mode = HvacMode::ALL
                    .iter()
                    .find(|m| m.name().eq_ignore_ascii_case(text))
                    .copied()
                    .or_else(|| {
                        text.parse::<usize>()
                            .ok()
                            .and_then(|i| HvacMode::ALL.get(i).copied())
                    });
                DptValue::HvacMode(mode.ok_or(DptError::Parse)?)
            }
            Codec::Rgb => DptValue::Rgb(parse_rgb(text)?),
        })
    }

    fn parse_bool(self, text: &str) -> Result<bool, DptError> {
        parse_bool(text, self.bool_labels())
    }

    /// Labels for `false` and `true` of DPT 1 subtypes.
    fn bool_labels(self) -> Option<(&'static str, &'static str)> {
        match (self.main, self.sub?) {
            (1, 1) => Some(("off", "on")),
            (1, 2) => Some(("false", "true")),
            (1, 3) => Some(("disable", "enable")),
            (1, 8) => Some(("up", "down")),
            (1, 9) => Some(("open", "close")),
            (1, 10) => Some(("stop", "start")),
            _ => None,
        }
    }

    /// Unit for common subtypes, e.g. `°C` for 9.001. Taken from ETS's `knx_master.xml`.
    pub fn unit(self) -> Option<&'static str> {
        Some(match (self.main, self.sub?) {
            (5, 1) | (5, 4) | (6, 1) | (8, 10) | (9, 7) => "%",
            (5, 3) | (8, 11) => "°",
            (7, 1) | (12, 1) | (13, 1) => "pulses",
            (7, 2) | (7, 5) => "s",
            (7, 13) | (9, 4) => "lx",
            (9, 1) | (14, 68) => "°C",
            (9, 2) => "K",
            (9, 5) => "m/s",
            (9, 6) => "Pa",
            (9, 8) => "ppm",
            (9, 20) => "mV",
            (9, 21) => "mA",
            (9, 22) => "W/m²",
            (9, 24) => "kW",
            (9, 25) => "l/h",
            (9, 27) => "°F",
            (9, 28) => "km/h",
            (12, 1200) => "l",
            (12, 1201) | (14, 76) => "m³",
            (13, 2) => "m³/h",
            (13, 10) => "Wh",
            (13, 13) => "kWh",
            (14, 19) => "A",
            (14, 27) => "V",
            (14, 33) => "Hz",
            (14, 56) => "W",
            _ => return None,
        })
    }

    /// Format a value for display, with labels and unit where known: `21.5 °C`, `on`, `50 %`.
    pub fn format<'a>(self, value: &'a DptValue) -> impl fmt::Display + 'a {
        Formatted { id: self, value }
    }
}

struct Formatted<'a> {
    id: DptId,
    value: &'a DptValue,
}

impl fmt::Display for Formatted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.value, self.id.bool_labels()) {
            (DptValue::Bool(b), Some((off, on))) => f.write_str(if *b { on } else { off })?,
            _ => self.value.fmt(f)?,
        }
        match self.id.unit() {
            Some(unit) => write!(f, " {unit}"),
            None => Ok(()),
        }
    }
}

fn parse_bool(text: &str, labels: Option<(&str, &str)>) -> Result<bool, DptError> {
    if let Some((off, on)) = labels {
        if text.eq_ignore_ascii_case(on) {
            return Ok(true);
        }
        if text.eq_ignore_ascii_case(off) {
            return Ok(false);
        }
    }
    match text.to_ascii_lowercase().as_str() {
        "1" | "on" | "true" => Ok(true),
        "0" | "off" | "false" => Ok(false),
        _ => Err(DptError::Parse),
    }
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn parse_time(text: &str) -> Result<TimeOfDay, DptError> {
    let (weekday, clock) = match text.split_once(' ') {
        Some((day, clock)) => {
            let idx = WEEKDAYS
                .iter()
                .position(|d| d.eq_ignore_ascii_case(day))
                .ok_or(DptError::Parse)?;
            (idx as u8 + 1, clock.trim())
        }
        None => (0, text),
    };
    let mut parts = clock
        .split(':')
        .map(|p| p.parse::<u8>().map_err(|_| DptError::Parse));
    let hour = parts.next().ok_or(DptError::Parse)??;
    let minute = parts.next().ok_or(DptError::Parse)??;
    let second = parts.next().transpose()?.unwrap_or(0);
    if parts.next().is_some() {
        return Err(DptError::Parse);
    }
    Ok(TimeOfDay {
        weekday,
        hour,
        minute,
        second,
    })
}

fn parse_date(text: &str) -> Result<Date, DptError> {
    let mut parts = text.split('-');
    let mut next = || parts.next().ok_or(DptError::Parse);
    let year = next()?.parse().map_err(|_| DptError::Parse)?;
    let month = next()?.parse().map_err(|_| DptError::Parse)?;
    let day = next()?.parse().map_err(|_| DptError::Parse)?;
    if parts.next().is_some() {
        return Err(DptError::Parse);
    }
    Ok(Date { year, month, day })
}

fn parse_rgb(text: &str) -> Result<Rgb, DptError> {
    if let Some(hex) = text.strip_prefix('#') {
        if hex.len() != 6 {
            return Err(DptError::Parse);
        }
        let raw = u32::from_str_radix(hex, 16).map_err(|_| DptError::Parse)?;
        let [_, r, g, b] = raw.to_be_bytes();
        return Ok(Rgb { r, g, b });
    }
    let mut parts = text
        .split(',')
        .map(|p| p.trim().parse::<u8>().map_err(|_| DptError::Parse));
    let mut next = || parts.next().ok_or(DptError::Parse)?;
    let rgb = Rgb {
        r: next()?,
        g: next()?,
        b: next()?,
    };
    if parts.next().is_some() {
        return Err(DptError::Parse);
    }
    Ok(rgb)
}

/// The kind of value a DPT carries; see [`DptId::value_kind`]. Text accepted by
/// [`DptId::parse_value`] is listed there per kind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ValueKind {
    /// On/off with the subtype's labels, e.g. `up`/`down` for 1.008.
    Bool {
        /// Label for `false`.
        off: &'static str,
        /// Label for `true`.
        on: &'static str,
    },
    /// DPT 2: control and value bits.
    Control,
    /// DPT 3: relative step (dimming, blinds).
    Step,
    /// Whole numbers in a range.
    Integer {
        /// Smallest value.
        min: i64,
        /// Largest value.
        max: i64,
    },
    /// Decimal numbers in a range.
    Float {
        /// Smallest value.
        min: f32,
        /// Largest value.
        max: f32,
    },
    /// DPT 10.001.
    TimeOfDay,
    /// DPT 11.001.
    Date,
    /// DPT 16: text of at most `max_len` characters.
    Text {
        /// Most characters.
        max_len: usize,
    },
    /// DPT 18.001.
    SceneControl,
    /// DPT 20.102.
    HvacMode,
    /// DPT 232.600.
    Rgb,
}

/// A decoded value of any supported datapoint type.
#[derive(Debug, Clone, PartialEq)]
pub enum DptValue {
    /// DPT 1.
    Bool(bool),
    /// DPT 2.
    Control(Control),
    /// DPT 3.
    Step(Step),
    /// DPT 5 (raw subtypes), 7, 12, and 17 (scene number 1–64).
    Unsigned(u32),
    /// DPT 6, 8, 13.
    Signed(i32),
    /// DPT 5.001, 5.003, 9, 14.
    Float(f32),
    /// DPT 10.001.
    TimeOfDay(TimeOfDay),
    /// DPT 11.001.
    Date(Date),
    /// DPT 16.
    String(String),
    /// DPT 18.001.
    SceneControl(SceneControl),
    /// DPT 20.102.
    HvacMode(HvacMode),
    /// DPT 232.600.
    Rgb(Rgb),
}

impl DptValue {
    fn as_i64(&self) -> Option<i64> {
        match *self {
            Self::Unsigned(v) => Some(v.into()),
            Self::Signed(v) => Some(v.into()),
            Self::Float(v) if v.is_finite() && v == (v as i64) as f32 => Some(v as i64),
            _ => None,
        }
    }

    fn as_f32(&self) -> Option<f32> {
        match *self {
            Self::Unsigned(v) => Some(v as f32),
            Self::Signed(v) => Some(v as f32),
            Self::Float(v) => Some(v),
            _ => None,
        }
    }
}

impl fmt::Display for DptValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(v) => write!(f, "{v}"),
            Self::Control(c) => write!(f, "{},{}", u8::from(c.control), u8::from(c.value)),
            Self::Step(s) => write!(f, "{}{}", if s.control { '+' } else { '-' }, s.step_code),
            Self::Unsigned(v) => write!(f, "{v}"),
            Self::Signed(v) => write!(f, "{v}"),
            Self::Float(v) => write_float(f, *v),
            Self::TimeOfDay(t) => {
                if let Some(day) = usize::from(t.weekday)
                    .checked_sub(1)
                    .and_then(|i| WEEKDAYS.get(i))
                {
                    write!(f, "{day} ")?;
                }
                write!(f, "{:02}:{:02}:{:02}", t.hour, t.minute, t.second)
            }
            Self::Date(d) => write!(f, "{:04}-{:02}-{:02}", d.year, d.month, d.day),
            Self::String(s) => f.write_str(s),
            Self::SceneControl(s) if s.learn => write!(f, "learn {}", s.scene),
            Self::SceneControl(s) => write!(f, "{}", s.scene),
            Self::HvacMode(m) => f.write_str(m.name()),
            Self::Rgb(c) => write!(f, "#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
        }
    }
}

/// Up to two decimals, trailing zeros removed: `21.5`, `50.2`, `100`.
fn write_float(f: &mut fmt::Formatter<'_>, v: f32) -> fmt::Result {
    let mut buf = String::new();
    fmt::write(&mut buf, format_args!("{v:.2}"))?;
    let trimmed = if buf.contains('.') {
        buf.trim_end_matches('0').trim_end_matches('.')
    } else {
        &buf
    };
    f.write_str(if trimmed == "-0" { "0" } else { trimmed })
}
