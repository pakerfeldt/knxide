//! The memory image of a download: what each byte of the product's segments should contain,
//! and which bytes are sent.
//!
//! # Which bytes are sent
//!
//! Derived from three recorded downloads (the product files only ever use `0x00` and `0xFF` in
//! a segment's `Mask`, so other values are untested):
//!
//! - In a **full** download a byte of product data is sent when its mask byte is not zero, or
//!   when the segment has no mask at all. A mask byte of `0x00` means "not part of the product
//!   image": the byte is skipped unless one of the rules below claims it.
//! - In a **partial** download no product byte is sent on its own account.
//! - In both, every byte that a parameter reference occupies is sent, whether the reference is
//!   in use or not. A reference in use writes its value; one that is not leaves the product's
//!   content in place. A parameter that occupies part of a byte claims the whole byte.
//! - Table bytes are sent as far as the tables reach; the rest of the space reserved for the
//!   tables is not ([`crate::tables`]).
//! - The two individual address bytes inside the address table are never sent.

use crate::config::Configuration;
use crate::error::Error;
use crate::mask::MaskProfile;
use crate::procedure::DownloadKind;
use crate::product::{
    ApplicationProgram, InvisibleParameters, ParameterKind, ParameterLocation, ParameterType,
};

/// One block of the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSegment {
    /// Absolute address of the first byte.
    pub address: u32,
    /// Content.
    pub data: Vec<u8>,
    /// Per byte: whether the download sends it.
    pub send: Vec<bool>,
}

impl ImageSegment {
    fn end(&self) -> u32 {
        self.address.saturating_add(self.data.len() as u32)
    }
}

/// A stretch of consecutive bytes to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Address of the first byte.
    pub address: u32,
    /// The bytes.
    pub data: Vec<u8>,
}

/// The memory image of one download.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Image {
    /// Segments in ascending address order, never overlapping.
    segments: Vec<ImageSegment>,
}

impl Image {
    /// The product's segments as a download of `kind` starts out with them.
    ///
    /// A segment with less data than its size is padded with zeros that are not sent. Product
    /// bytes whose mask is zero are replaced by zero (from behaviour notes; without effect on
    /// the recorded downloads).
    pub fn from_program(
        program: &ApplicationProgram,
        kind: DownloadKind,
        profile: &MaskProfile,
    ) -> Result<Self, Error> {
        let mut segments = Vec::with_capacity(program.segments.len());
        for segment in &program.segments {
            let size = usize::try_from(segment.size)
                .ok()
                .filter(|size| *size <= 0x1_0000)
                .ok_or_else(|| Error::OutOfRange(format!("size of segment {}", segment.id)))?;
            let given = segment.data.as_deref().unwrap_or_default();
            let length = size.max(given.len());
            // Standard memory is addressed with 16 bits.
            if u64::from(segment.address) + length as u64 > 0x1_0000 {
                return Err(Error::OutOfRange(format!(
                    "segment {} ends beyond address FFFF",
                    segment.id
                )));
            }
            let mut data = vec![0u8; length];
            let mut send = vec![false; length];
            for (at, byte) in given.iter().enumerate() {
                let valid = match &segment.mask {
                    Some(mask) => mask.get(at).is_some_and(|m| *m != 0),
                    None => true,
                };
                if let (true, Some(slot), Some(flag)) = (valid, data.get_mut(at), send.get_mut(at))
                {
                    *slot = *byte;
                    *flag = kind.full;
                }
            }
            segments.push(ImageSegment {
                address: segment.address,
                data,
                send,
            });
        }
        segments.sort_by_key(|segment| segment.address);
        for pair in segments.windows(2) {
            if let [first, second] = pair
                && first.end() > second.address
            {
                return Err(Error::Unsupported(format!(
                    "overlapping segments at {:04X}",
                    second.address
                )));
            }
        }
        let mut image = Self { segments };
        for offset in 0..2 {
            image.set_send(u32::from(profile.individual_address) + offset, false);
        }
        Ok(image)
    }

    /// The segments, in ascending address order.
    pub fn segments(&self) -> &[ImageSegment] {
        &self.segments
    }

    fn locate(&self, address: u32) -> Option<(usize, usize)> {
        let at = self
            .segments
            .partition_point(|segment| segment.end() <= address);
        let segment = self.segments.get(at)?;
        let offset = usize::try_from(address.checked_sub(segment.address)?).ok()?;
        (offset < segment.data.len()).then_some((at, offset))
    }

    /// The content of `address`, if the image covers it.
    pub fn byte(&self, address: u32) -> Option<u8> {
        let (segment, offset) = self.locate(address)?;
        self.segments.get(segment)?.data.get(offset).copied()
    }

    /// Whether the download sends the byte at `address`.
    pub fn is_sent(&self, address: u32) -> bool {
        self.locate(address)
            .and_then(|(segment, offset)| self.segments.get(segment)?.send.get(offset).copied())
            .unwrap_or(false)
    }

    /// Set the content of `address` and mark it for sending.
    pub(crate) fn write(&mut self, address: u32, value: u8) -> Result<(), Error> {
        let (segment, offset) = self.locate(address).ok_or_else(|| {
            Error::OutOfRange(format!("address {address:04X} is not in the image"))
        })?;
        if let Some(segment) = self.segments.get_mut(segment) {
            if let Some(slot) = segment.data.get_mut(offset) {
                *slot = value;
            }
            if let Some(flag) = segment.send.get_mut(offset) {
                *flag = true;
            }
        }
        Ok(())
    }

    /// Mark or unmark `address` for sending. Addresses outside the image are ignored.
    pub(crate) fn set_send(&mut self, address: u32, send: bool) {
        if let Some((segment, offset)) = self.locate(address)
            && let Some(flag) = self
                .segments
                .get_mut(segment)
                .and_then(|segment| segment.send.get_mut(offset))
        {
            *flag = send;
        }
    }

    /// The bytes to send within `size` bytes from `address`, as stretches of consecutive
    /// addresses in ascending order. Segments that touch form one stretch.
    pub fn runs(&self, address: u32, size: u32) -> Vec<Run> {
        let end = address.saturating_add(size);
        let mut runs: Vec<Run> = Vec::new();
        for segment in &self.segments {
            if segment.end() <= address || segment.address >= end {
                continue;
            }
            for (offset, (byte, send)) in segment.data.iter().zip(&segment.send).enumerate() {
                let here = segment.address.saturating_add(offset as u32);
                if !*send || here < address || here >= end {
                    continue;
                }
                match runs.last_mut() {
                    Some(run) if run.address.saturating_add(run.data.len() as u32) == here => {
                        run.data.push(*byte);
                    }
                    _ => runs.push(Run {
                        address: here,
                        data: vec![*byte],
                    }),
                }
            }
        }
        runs
    }

    /// Whether any byte of the image lies within `size` bytes from `address`.
    pub fn covers_any(&self, address: u32, size: u32) -> bool {
        let end = address.saturating_add(size);
        self.segments
            .iter()
            .any(|segment| segment.end() > address && segment.address < end)
    }

    /// Place the parameters of `configuration` into the image.
    ///
    /// For every parameter reference, in document order, whose parameter has a memory location
    /// and a type that occupies bits: a reference in use writes its value as a bit field
    /// starting at the parameter's bit position, most significant bit first (so integers are
    /// big-endian); one that is not in use writes nothing. Either way the bytes the field
    /// touches are marked for sending.
    ///
    /// Verified against three recorded downloads for unsigned integers and enumerations. Signed
    /// integers and text are encoded as the behaviour notes describe and are unverified.
    pub fn apply_parameters(
        &mut self,
        program: &ApplicationProgram,
        configuration: &Configuration,
    ) -> Result<(), Error> {
        for (index, reference) in program.parameter_refs.iter().enumerate() {
            let parameter = program
                .parameter(&reference.parameter)
                .ok_or_else(|| Error::UnknownReference(reference.parameter.clone()))?;
            let ParameterLocation::Memory { segment, bit } = &parameter.location else {
                continue;
            };
            let kind = program
                .parameter_type(&parameter.parameter_type)
                .ok_or_else(|| Error::UnknownReference(parameter.parameter_type.clone()))?;
            if kind.size_in_bit == 0 {
                continue;
            }
            let segment = program
                .segment(segment)
                .ok_or_else(|| Error::UnknownReference(segment.clone()))?;
            let out_of_range = || {
                Error::OutOfRange(format!(
                    "parameter {} lies outside segment {}",
                    parameter.id, segment.id
                ))
            };
            let first_bit = u64::from(segment.address) * 8 + u64::from(*bit);
            let end_bit = first_bit + u64::from(kind.size_in_bit);
            let first_byte = u32::try_from(first_bit / 8).map_err(|_| out_of_range())?;
            let end_byte = u32::try_from(end_bit.div_ceil(8)).map_err(|_| out_of_range())?;
            if end_byte > segment.address.saturating_add(segment.size)
                || (first_byte..end_byte).any(|address| self.locate(address).is_none())
            {
                return Err(out_of_range());
            }
            let active = configuration.parameter_active(index);
            if active {
                let value = configuration.value(program, index)?;
                let bits = encode(kind, value, program.options.zero_terminate_text)
                    .map_err(|error| annotate(error, &reference.id))?;
                for (position, bit) in (first_bit..end_bit).zip(bits) {
                    self.put_bit(position, bit);
                }
            }
            if active || program.options.invisible_parameters == InvisibleParameters::Background {
                for address in first_byte..end_byte {
                    self.set_send(address, true);
                }
            }
        }
        Ok(())
    }

    /// Set one bit; `position` counts bits from address 0, most significant bit of a byte first.
    fn put_bit(&mut self, position: u64, bit: bool) {
        let Ok(address) = u32::try_from(position / 8) else {
            return;
        };
        let mask = 0x80u8 >> (position % 8);
        if let Some((segment, offset)) = self.locate(address)
            && let Some(byte) = self
                .segments
                .get_mut(segment)
                .and_then(|segment| segment.data.get_mut(offset))
        {
            *byte = if bit { *byte | mask } else { *byte & !mask };
        }
    }
}

fn annotate(error: Error, reference: &str) -> Error {
    match error {
        Error::Invalid { what, value } => Error::Invalid {
            what,
            value: format!("{value} (parameter reference {reference})"),
        },
        Error::OutOfRange(message) => {
            Error::OutOfRange(format!("{message} (parameter reference {reference})"))
        }
        other => other,
    }
}

/// The bits of `value` as a parameter of type `kind` stores them, most significant first.
pub(crate) fn encode(
    kind: &ParameterType,
    value: &str,
    zero_terminate: bool,
) -> Result<Vec<bool>, Error> {
    let bits = kind.size_in_bit;
    match &kind.kind {
        ParameterKind::Number { signed } => integer_bits(value, bits, *signed),
        ParameterKind::Restriction => integer_bits(value, bits, false),
        ParameterKind::Text => Ok(text_bits(value, bits, zero_terminate)),
        ParameterKind::None => Ok(Vec::new()),
        ParameterKind::Binary(_) => Err(Error::Unsupported(format!(
            "byte string parameter in memory ({})",
            kind.id
        ))),
        ParameterKind::Other(name) => Err(Error::Unsupported(format!(
            "parameter type {name} ({})",
            kind.id
        ))),
    }
}

/// Parse a value as ETS writes integers: plain decimal, or a decimal fraction without a
/// fractional part.
fn parse_integer(value: &str) -> Option<i128> {
    let text = value.trim();
    if let Ok(number) = text.parse::<i128>() {
        return Some(number);
    }
    let number = text.parse::<f64>().ok()?;
    (number.fract() == 0.0 && number.abs() < 1e18).then_some(number as i128)
}

fn integer_bits(value: &str, bits: u32, signed: bool) -> Result<Vec<bool>, Error> {
    if bits == 0 {
        return Ok(Vec::new());
    }
    if bits > 64 {
        return Err(Error::Unsupported(format!("{bits}-bit integer parameter")));
    }
    let number = parse_integer(value).ok_or_else(|| Error::Invalid {
        what: "integer parameter value",
        value: value.chars().take(40).collect(),
    })?;
    let (low, high) = if signed {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    if number < low || number > high {
        return Err(Error::OutOfRange(format!(
            "value {number} does not fit {bits} bits"
        )));
    }
    // Two's complement of the field's width for negative numbers.
    let stored = (number as u128) & ((1u128 << bits) - 1);
    Ok((0..bits)
        .map(|at| stored >> (bits - 1 - at) & 1 == 1)
        .collect())
}

/// Text as ISO 8859-1 bytes, cut at a character boundary to fit and padded with zero bytes.
/// Characters outside ISO 8859-1 become `?`.
fn text_bits(value: &str, bits: u32, zero_terminate: bool) -> Vec<bool> {
    let room = (bits / 8) as usize;
    let room_for_text = room.saturating_sub(usize::from(zero_terminate));
    let mut bytes: Vec<u8> = value
        .chars()
        .take(room_for_text)
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect();
    bytes.resize(room, 0);
    let mut out: Vec<bool> = bytes
        .iter()
        .flat_map(|byte| (0..8).map(move |at| byte >> (7 - at) & 1 == 1))
        .collect();
    out.resize(bits as usize, false);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(bits: &[bool]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0u8, |byte, (at, bit)| byte | (u8::from(*bit) << (7 - at)))
            })
            .collect()
    }

    #[test]
    fn integers() {
        assert_eq!(
            pack(&integer_bits("4660", 16, false).unwrap()),
            [0x12, 0x34]
        );
        assert_eq!(pack(&integer_bits("5", 3, false).unwrap()), [0b1010_0000]);
        assert_eq!(pack(&integer_bits("-2", 8, true).unwrap()), [0xFE]);
        assert_eq!(pack(&integer_bits("-1", 4, true).unwrap()), [0xF0]);
        assert_eq!(pack(&integer_bits("3.0", 8, false).unwrap()), [3]);
        assert_eq!(
            pack(&integer_bits("18446744073709551615", 64, false).unwrap()),
            [0xFF; 8]
        );
        assert!(integer_bits("256", 8, false).is_err());
        assert!(integer_bits("-1", 8, false).is_err());
        assert!(integer_bits("128", 8, true).is_err());
        assert!(integer_bits("1.5", 8, false).is_err());
        assert!(integer_bits("abc", 8, false).is_err());
        assert!(integer_bits("1", 65, false).is_err());
    }

    #[test]
    fn text() {
        assert_eq!(pack(&text_bits("Ab", 32, false)), [b'A', b'b', 0, 0]);
        assert_eq!(pack(&text_bits("Abcdef", 32, false)), *b"Abcd");
        assert_eq!(pack(&text_bits("Abcdef", 32, true)), [b'A', b'b', b'c', 0]);
        assert_eq!(pack(&text_bits("ä€", 24, false)), [0xE4, b'?', 0]);
    }
}
