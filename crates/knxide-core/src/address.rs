//! KNX individual and group addresses.
//!
//! Both are 16-bit values on the wire. The dotted and slashed forms are presentation only.
//! See KNX spec 3/3/2 (Data Link Layer General) §1.4 and 3/5/1 (Resources) for address formats.

use core::fmt;
use core::str::FromStr;

/// Error returned when an address is out of range or cannot be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressError;

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid KNX address")
    }
}

impl core::error::Error for AddressError {}

/// Individual (physical) address of a device: `area.line.device`, 4/4/8 bits.
///
/// Spec: 3/3/2 §1.4.2 (Individual Address).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct IndividualAddress(u16);

impl IndividualAddress {
    /// `0.0.0`. Sent as the source of `L_Data.req`; the tunnelling server substitutes the
    /// tunnel's own address (spec 3/8/4 §2.2.4).
    pub const UNSET: Self = Self(0);

    /// Build from area (0–15), line (0–15) and device (0–255).
    pub const fn new(area: u8, line: u8, device: u8) -> Option<Self> {
        if area > 0x0F || line > 0x0F {
            return None;
        }
        Some(Self(
            ((area as u16) << 12) | ((line as u16) << 8) | device as u16,
        ))
    }

    /// Build from the raw 16-bit wire value.
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    /// The raw 16-bit wire value.
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// Area, the top 4 bits.
    pub const fn area(self) -> u8 {
        (self.0 >> 12) as u8
    }

    /// Line, bits 11–8.
    pub const fn line(self) -> u8 {
        ((self.0 >> 8) & 0x0F) as u8
    }

    /// Device, the low 8 bits.
    pub const fn device(self) -> u8 {
        (self.0 & 0xFF) as u8
    }
}

impl fmt::Display for IndividualAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.area(), self.line(), self.device())
    }
}

impl FromStr for IndividualAddress {
    type Err = AddressError;

    /// Parses `area.line.device`, e.g. `1.1.240`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.trim().split('.');
        let area = parse_part(parts.next())?;
        let line = parse_part(parts.next())?;
        let device = parse_part(parts.next())?;
        if parts.next().is_some() {
            return Err(AddressError);
        }
        let (Ok(area), Ok(line), Ok(device)) =
            (u8::try_from(area), u8::try_from(line), u8::try_from(device))
        else {
            return Err(AddressError);
        };
        Self::new(area, line, device).ok_or(AddressError)
    }
}

/// Group address: a 16-bit value shared by sensors and actuators for one function.
///
/// Spec: 3/3/2 §1.4.3 (Group Address). `0/0/0` is the broadcast address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct GroupAddress(u16);

/// How a [`GroupAddress`] is written. The wire value is the same for all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GroupAddressStyle {
    /// `main/middle/sub`, 5/3/8 bits, e.g. `1/2/3`.
    #[default]
    ThreeLevel,
    /// `main/sub`, 5/11 bits, e.g. `1/515`.
    TwoLevel,
    /// Plain 16-bit number, e.g. `2563`.
    Free,
}

impl GroupAddress {
    /// Build a 3-level address: main (0–31), middle (0–7), sub (0–255).
    pub const fn new(main: u8, middle: u8, sub: u8) -> Option<Self> {
        if main > 0x1F || middle > 0x07 {
            return None;
        }
        Some(Self(
            ((main as u16) << 11) | ((middle as u16) << 8) | sub as u16,
        ))
    }

    /// Build a 2-level address: main (0–31), sub (0–2047).
    pub const fn new_two_level(main: u8, sub: u16) -> Option<Self> {
        if main > 0x1F || sub > 0x07FF {
            return None;
        }
        Some(Self(((main as u16) << 11) | sub))
    }

    /// Build from the raw 16-bit wire value (free-style address).
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    /// The raw 16-bit wire value.
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// Main group, the top 5 bits.
    pub const fn main(self) -> u8 {
        (self.0 >> 11) as u8
    }

    /// Middle group of a 3-level address, bits 10–8.
    pub const fn middle(self) -> u8 {
        ((self.0 >> 8) & 0x07) as u8
    }

    /// Sub group of a 3-level address, the low 8 bits.
    pub const fn sub(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// Sub group of a 2-level address, the low 11 bits.
    pub const fn sub_two_level(self) -> u16 {
        self.0 & 0x07FF
    }

    /// Format in the given style. `Display` uses [`GroupAddressStyle::ThreeLevel`].
    pub const fn display(self, style: GroupAddressStyle) -> impl fmt::Display {
        StyledGroupAddress(self, style)
    }
}

struct StyledGroupAddress(GroupAddress, GroupAddressStyle);

impl fmt::Display for StyledGroupAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ga = self.0;
        match self.1 {
            GroupAddressStyle::ThreeLevel => {
                write!(f, "{}/{}/{}", ga.main(), ga.middle(), ga.sub())
            }
            GroupAddressStyle::TwoLevel => write!(f, "{}/{}", ga.main(), ga.sub_two_level()),
            GroupAddressStyle::Free => write!(f, "{}", ga.raw()),
        }
    }
}

impl fmt::Display for GroupAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        StyledGroupAddress(*self, GroupAddressStyle::ThreeLevel).fmt(f)
    }
}

impl FromStr for GroupAddress {
    type Err = AddressError;

    /// Parses any style: `1/2/3`, `1/515` or `2563`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.trim().split('/');
        let first = parse_part(parts.next())?;
        let second = parts.next().map(|p| parse_part(Some(p))).transpose()?;
        let third = parts.next().map(|p| parse_part(Some(p))).transpose()?;
        if parts.next().is_some() {
            return Err(AddressError);
        }
        match (second, third) {
            (None, _) => u16::try_from(first).map(Self).map_err(|_| AddressError),
            (Some(sub), None) => {
                let (Ok(main), Ok(sub)) = (u8::try_from(first), u16::try_from(sub)) else {
                    return Err(AddressError);
                };
                Self::new_two_level(main, sub).ok_or(AddressError)
            }
            (Some(middle), Some(sub)) => {
                let (Ok(main), Ok(middle), Ok(sub)) =
                    (u8::try_from(first), u8::try_from(middle), u8::try_from(sub))
                else {
                    return Err(AddressError);
                };
                Self::new(main, middle, sub).ok_or(AddressError)
            }
        }
    }
}

fn parse_part(part: Option<&str>) -> Result<u32, AddressError> {
    let part = part.ok_or(AddressError)?;
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AddressError);
    }
    part.parse().map_err(|_| AddressError)
}

/// Destination of an `L_Data` frame. The cEMI address-type flag follows from the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Destination {
    /// Point-to-point to one device.
    Individual(IndividualAddress),
    /// Multicast to a group (or broadcast with `0/0/0`).
    Group(GroupAddress),
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Individual(a) => a.fmt(f),
            Self::Group(a) => a.fmt(f),
        }
    }
}

impl From<IndividualAddress> for Destination {
    fn from(a: IndividualAddress) -> Self {
        Self::Individual(a)
    }
}

impl From<GroupAddress> for Destination {
    fn from(a: GroupAddress) -> Self {
        Self::Group(a)
    }
}

/// A 3-level [`GroupAddress`] checked at compile time: `ga!(1/2/3)`.
///
/// ```
/// use knxide_core::ga;
/// assert_eq!(ga!(1/2/3).raw(), 0x0A03);
/// ```
#[macro_export]
macro_rules! ga {
    ($main:literal / $middle:literal / $sub:literal) => {
        const {
            match $crate::address::GroupAddress::new($main, $middle, $sub) {
                Some(ga) => ga,
                None => panic!("group address out of range"),
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn individual_address_layout() {
        let ia = IndividualAddress::new(1, 1, 240).unwrap();
        assert_eq!(ia.raw(), 0x11F0);
        assert_eq!(ia.to_string(), "1.1.240");
        assert_eq!("1.1.240".parse::<IndividualAddress>(), Ok(ia));
        assert_eq!(IndividualAddress::from_raw(0xFFFF).to_string(), "15.15.255");
    }

    #[test]
    fn individual_address_rejects_bad_input() {
        for s in [
            "16.0.0", "0.16.0", "0.0.256", "1.1", "1.1.1.1", "", "a.b.c", "1..1", "+1.1.1",
        ] {
            assert!(s.parse::<IndividualAddress>().is_err(), "{s}");
        }
    }

    #[test]
    fn group_address_styles() {
        let ga = GroupAddress::new(1, 2, 3).unwrap();
        assert_eq!(ga.raw(), 0x0A03);
        assert_eq!(ga.to_string(), "1/2/3");
        assert_eq!(ga.display(GroupAddressStyle::TwoLevel).to_string(), "1/515");
        assert_eq!(ga.display(GroupAddressStyle::Free).to_string(), "2563");
        for s in ["1/2/3", "1/515", "2563"] {
            assert_eq!(s.parse::<GroupAddress>(), Ok(ga), "{s}");
        }
        assert_eq!(GroupAddress::from_raw(0xFFFF).to_string(), "31/7/255");
    }

    #[test]
    fn group_address_rejects_bad_input() {
        for s in [
            "32/0/0", "0/8/0", "0/0/256", "32/0", "0/2048", "65536", "1/2/3/4", "1//3", "",
        ] {
            assert!(s.parse::<GroupAddress>().is_err(), "{s}");
        }
    }

    #[test]
    fn ga_macro() {
        const GA: GroupAddress = ga!(31 / 7 / 255);
        assert_eq!(GA.raw(), 0xFFFF);
    }
}
