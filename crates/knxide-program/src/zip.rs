//! A small ZIP reader, enough for project archives: the central directory, entries that are
//! stored or deflated, and their CRC. Format: PKWARE APPNOTE 6.3; deflate: RFC 1951.
//!
//! Everything is bounded: the number of entries, the size of one entry and what a deflate
//! stream may expand to. Encrypted entries are recognised and refused; ZIP64 and archives that
//! span several files are refused. No input makes the reader panic.

use std::fmt;

/// Most entries an archive may list.
pub(crate) const MAX_ENTRIES: usize = 20_000;
/// Longest entry name accepted, in bytes.
const MAX_NAME: usize = 1024;

/// Signature of the end-of-central-directory record.
const END_SIGNATURE: u32 = 0x0605_4B50;
/// Signature of a central directory file header.
const CENTRAL_SIGNATURE: u32 = 0x0201_4B50;
/// Signature of a local file header.
const LOCAL_SIGNATURE: u32 = 0x0403_4B50;
/// Fixed part of the end-of-central-directory record.
const END_SIZE: usize = 22;
/// Fixed part of a central directory file header.
const CENTRAL_SIZE: usize = 46;
/// Fixed part of a local file header.
const LOCAL_SIZE: usize = 30;

/// General purpose flag bit 0: the entry is encrypted.
const FLAG_ENCRYPTED: u16 = 0x0001;
/// General purpose flag bit 6: strong encryption.
const FLAG_STRONG_ENCRYPTION: u16 = 0x0040;
/// Compression method 0: stored.
const METHOD_STORED: u16 = 0;
/// Compression method 8: deflate.
const METHOD_DEFLATE: u16 = 8;
/// Compression method 99: AES encryption, with the real method in an extra field.
const METHOD_AES: u16 = 99;

/// Why an archive or one of its entries cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ZipError {
    /// Not a ZIP archive, or damaged. The string says what is wrong.
    Malformed(&'static str),
    /// A feature this reader does not have. The string names it.
    Unsupported(&'static str),
    /// The entry is encrypted.
    Encrypted,
    /// The entry, or the archive's list of entries, exceeds a limit.
    TooLarge,
    /// The entry's content does not match its checksum or its stated size.
    Corrupt,
}

impl fmt::Display for ZipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "not a readable ZIP archive: {what}"),
            Self::Unsupported(what) => write!(f, "ZIP feature not supported: {what}"),
            Self::Encrypted => f.write_str("the entry is encrypted"),
            Self::TooLarge => f.write_str("the entry is larger than allowed"),
            Self::Corrupt => f.write_str("the entry does not match its checksum"),
        }
    }
}

/// One entry of the central directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The entry's path inside the archive, `/`-separated.
    pub name: String,
    flags: u16,
    method: u16,
    crc32: u32,
    compressed_size: usize,
    /// Size of the content once unpacked, as the directory states it.
    pub size: usize,
    header_offset: usize,
}

impl Entry {
    /// Whether the entry is encrypted (and so cannot be read here).
    pub(crate) fn is_encrypted(&self) -> bool {
        self.flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0 || self.method == METHOD_AES
    }
}

/// An archive held in memory.
#[derive(Debug, Clone)]
pub(crate) struct Archive<'a> {
    bytes: &'a [u8],
    entries: Vec<Entry>,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let chunk = bytes.get(at..)?.first_chunk::<2>()?;
    Some(u16::from_le_bytes(*chunk))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let chunk = bytes.get(at..)?.first_chunk::<4>()?;
    Some(u32::from_le_bytes(*chunk))
}

impl<'a> Archive<'a> {
    /// Read the central directory of `bytes`.
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Self, ZipError> {
        let truncated = ZipError::Malformed("truncated central directory");
        let end = find_end(bytes).ok_or(ZipError::Malformed("no end of central directory"))?;
        let field16 = |at: usize| u16_at(bytes, end + at).ok_or(truncated.clone());
        let (disk, directory_disk) = (field16(4)?, field16(6)?);
        let (here, total) = (field16(8)?, field16(10)?);
        let directory_size = u32_at(bytes, end + 12).ok_or(truncated.clone())?;
        let directory_offset = u32_at(bytes, end + 16).ok_or(truncated.clone())?;
        if total == u16::MAX || directory_size == u32::MAX || directory_offset == u32::MAX {
            return Err(ZipError::Unsupported("ZIP64"));
        }
        if disk != 0 || directory_disk != 0 || here != total {
            return Err(ZipError::Unsupported("archives in several parts"));
        }
        if usize::from(total) > MAX_ENTRIES {
            return Err(ZipError::TooLarge);
        }
        let mut at = usize::try_from(directory_offset).map_err(|_| truncated.clone())?;
        let mut entries = Vec::with_capacity(usize::from(total));
        for _ in 0..total {
            if u32_at(bytes, at) != Some(CENTRAL_SIGNATURE) {
                return Err(ZipError::Malformed("bad central directory entry"));
            }
            let field = |offset: usize| u16_at(bytes, at + offset).ok_or(truncated.clone());
            let (flags, method) = (field(8)?, field(10)?);
            let crc32 = u32_at(bytes, at + 16).ok_or(truncated.clone())?;
            let compressed = u32_at(bytes, at + 20).ok_or(truncated.clone())?;
            let size = u32_at(bytes, at + 24).ok_or(truncated.clone())?;
            let name_length = usize::from(field(28)?);
            let extra_length = usize::from(field(30)?);
            let comment_length = usize::from(field(32)?);
            let offset = u32_at(bytes, at + 42).ok_or(truncated.clone())?;
            if compressed == u32::MAX || size == u32::MAX || offset == u32::MAX {
                return Err(ZipError::Unsupported("ZIP64"));
            }
            if name_length > MAX_NAME {
                return Err(ZipError::TooLarge);
            }
            let name = bytes
                .get(at + CENTRAL_SIZE..)
                .and_then(|rest| rest.get(..name_length))
                .ok_or(truncated.clone())?;
            entries.push(Entry {
                // Names that are not UTF-8 never match a path this crate looks for.
                name: String::from_utf8_lossy(name).replace('\\', "/"),
                flags,
                method,
                crc32,
                compressed_size: usize::try_from(compressed).map_err(|_| ZipError::TooLarge)?,
                size: usize::try_from(size).map_err(|_| ZipError::TooLarge)?,
                header_offset: usize::try_from(offset).map_err(|_| ZipError::TooLarge)?,
            });
            at = at
                .checked_add(CENTRAL_SIZE + name_length + extra_length + comment_length)
                .ok_or(truncated.clone())?;
        }
        Ok(Self { bytes, entries })
    }

    /// The entries, in directory order.
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry called `name`.
    pub(crate) fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// Unpack `entry`. Fails when it is encrypted, larger than `limit`, compressed with
    /// anything but deflate, or when the content does not match the directory's size and CRC.
    pub(crate) fn read(&self, entry: &Entry, limit: usize) -> Result<Vec<u8>, ZipError> {
        if entry.is_encrypted() {
            return Err(ZipError::Encrypted);
        }
        if entry.size > limit {
            return Err(ZipError::TooLarge);
        }
        let truncated = ZipError::Malformed("truncated entry");
        let header = entry.header_offset;
        if u32_at(self.bytes, header) != Some(LOCAL_SIGNATURE) {
            return Err(ZipError::Malformed("bad local file header"));
        }
        // Name and extra field lengths of the local header may differ from the directory's.
        let name_length = usize::from(u16_at(self.bytes, header + 26).ok_or(truncated.clone())?);
        let extra_length = usize::from(u16_at(self.bytes, header + 28).ok_or(truncated.clone())?);
        let start = header
            .checked_add(LOCAL_SIZE + name_length + extra_length)
            .ok_or(truncated.clone())?;
        let packed = self
            .bytes
            .get(start..)
            .and_then(|rest| rest.get(..entry.compressed_size))
            .ok_or(truncated)?;
        let content = match entry.method {
            METHOD_STORED => packed.to_vec(),
            METHOD_DEFLATE => inflate(packed, entry.size)?,
            _ => return Err(ZipError::Unsupported("compression method")),
        };
        if content.len() != entry.size || crc32(&content) != entry.crc32 {
            return Err(ZipError::Corrupt);
        }
        Ok(content)
    }
}

/// Offset of the end-of-central-directory record: the last place where its signature is
/// followed by a comment that ends exactly at the end of the file.
fn find_end(bytes: &[u8]) -> Option<usize> {
    let last = bytes.len().checked_sub(END_SIZE)?;
    let first = last.saturating_sub(usize::from(u16::MAX));
    (first..=last).rev().find(|at| {
        u32_at(bytes, *at) == Some(END_SIGNATURE)
            && u16_at(bytes, at + 20).map(usize::from) == Some(last - at)
    })
}

/// CRC-32 as ZIP uses it (polynomial `0xEDB88320`, reflected, inverted in and out).
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let table: [u32; 256] = std::array::from_fn(|index| {
        (0..8).fold(index as u32, |value, _| {
            if value & 1 != 0 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            }
        })
    });
    let mut crc = u32::MAX;
    for byte in bytes {
        let index = usize::from((crc as u8) ^ byte);
        crc = table.get(index).copied().unwrap_or(0) ^ (crc >> 8);
    }
    !crc
}

// ---------------------------------------------------------------------------------------------
// Inflate (RFC 1951)
// ---------------------------------------------------------------------------------------------

const BAD_STREAM: ZipError = ZipError::Malformed("bad deflate stream");

/// Longest Huffman code in a deflate stream.
const MAX_CODE_LENGTH: usize = 15;
/// Base match length per length symbol 257–285. RFC 1951 §3.2.5.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits per length symbol.
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Base distance per distance symbol 0–29.
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits per distance symbol.
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Order in which the code length code lengths are stored. RFC 1951 §3.2.7.
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Reads bits least significant first.
struct Bits<'a> {
    input: &'a [u8],
    position: usize,
    buffer: u64,
    count: u32,
}

impl Bits<'_> {
    fn refill(&mut self) {
        while self.count <= 56 {
            let Some(byte) = self.input.get(self.position) else {
                return;
            };
            self.buffer |= u64::from(*byte) << self.count;
            self.count += 8;
            self.position += 1;
        }
    }

    /// The next `count` bits (at most 32) without consuming them; bits past the end are zero.
    fn peek(&mut self, count: u32) -> u32 {
        self.refill();
        (self.buffer & ((1u64 << count) - 1)) as u32
    }

    fn consume(&mut self, count: u32) -> Result<(), ZipError> {
        if count > self.count {
            return Err(BAD_STREAM);
        }
        self.buffer >>= count;
        self.count -= count;
        Ok(())
    }

    fn take(&mut self, count: u32) -> Result<u32, ZipError> {
        let value = self.peek(count);
        self.consume(count)?;
        Ok(value)
    }

    /// Drop the bits up to the next byte boundary.
    fn align(&mut self) {
        let partial = self.count % 8;
        self.buffer >>= partial;
        self.count -= partial;
    }

    /// Copy `length` whole bytes to `out`. Call only at a byte boundary.
    fn copy_bytes(&mut self, length: usize, out: &mut Vec<u8>) -> Result<(), ZipError> {
        let mut left = length;
        while left > 0 && self.count >= 8 {
            out.push(self.buffer as u8);
            self.buffer >>= 8;
            self.count -= 8;
            left -= 1;
        }
        let rest = self
            .input
            .get(self.position..)
            .and_then(|rest| rest.get(..left))
            .ok_or(BAD_STREAM)?;
        out.extend_from_slice(rest);
        self.position += left;
        Ok(())
    }
}

/// A Huffman code as a lookup table indexed by the next `bits` bits of the stream. An entry
/// is `symbol << 4 | code length`; 0 marks a bit pattern no code starts with.
struct Huffman {
    table: Vec<u16>,
    bits: u32,
}

impl Huffman {
    /// Build the canonical code for `lengths` (one per symbol, 0 = unused). RFC 1951 §3.2.2.
    fn new(lengths: &[u8]) -> Result<Self, ZipError> {
        let mut counts = [0u32; MAX_CODE_LENGTH + 1];
        for length in lengths {
            *counts.get_mut(usize::from(*length)).ok_or(BAD_STREAM)? += 1;
        }
        let longest = (1..=MAX_CODE_LENGTH)
            .rev()
            .find(|length| counts.get(*length).is_some_and(|count| *count > 0))
            .unwrap_or(0);
        // First code of each length; more codes than the lengths allow is an error.
        let mut next = [0u32; MAX_CODE_LENGTH + 2];
        let mut code = 0u32;
        for length in 1..=MAX_CODE_LENGTH {
            let count = counts.get(length).copied().unwrap_or(0);
            if let Some(slot) = next.get_mut(length) {
                *slot = code;
            }
            code = (code + count) << 1;
            if code > 1 << (length + 1) {
                return Err(BAD_STREAM);
            }
        }
        let bits = longest as u32;
        let mut table = vec![0u16; 1 << bits];
        for (symbol, length) in lengths.iter().enumerate() {
            let length = u32::from(*length);
            if length == 0 {
                continue;
            }
            let slot = next.get_mut(length as usize).ok_or(BAD_STREAM)?;
            let code = *slot;
            *slot += 1;
            // The stream carries codes most significant bit first; the table is indexed by
            // bits as they are read, least significant first.
            let reversed = code.reverse_bits() >> (32 - length);
            let entry = u16::try_from(symbol << 4).map_err(|_| BAD_STREAM)? | length as u16;
            let mut index = reversed as usize;
            while let Some(place) = table.get_mut(index) {
                *place = entry;
                index += 1 << length;
            }
        }
        Ok(Self { table, bits })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<usize, ZipError> {
        let index = bits.peek(self.bits) as usize;
        let entry = self.table.get(index).copied().unwrap_or(0);
        let length = u32::from(entry & 0x0F);
        if length == 0 {
            return Err(BAD_STREAM);
        }
        bits.consume(length)?;
        Ok(usize::from(entry >> 4))
    }
}

/// The fixed codes of block type 1. RFC 1951 §3.2.6.
fn fixed_codes() -> Result<(Huffman, Huffman), ZipError> {
    let mut literal = [8u8; 288];
    for (symbol, length) in literal.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Ok((Huffman::new(&literal)?, Huffman::new(&[5u8; 30])?))
}

/// The codes a dynamic block (type 2) starts with. RFC 1951 §3.2.7.
fn dynamic_codes(bits: &mut Bits<'_>) -> Result<(Huffman, Huffman), ZipError> {
    let literal_count = bits.take(5)? as usize + 257;
    let distance_count = bits.take(5)? as usize + 1;
    let length_count = bits.take(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return Err(BAD_STREAM);
    }
    let mut code_lengths = [0u8; 19];
    for place in CODE_LENGTH_ORDER.iter().take(length_count) {
        if let Some(slot) = code_lengths.get_mut(*place) {
            *slot = bits.take(3)? as u8;
        }
    }
    let code = Huffman::new(&code_lengths)?;
    let total = literal_count + distance_count;
    let mut lengths: Vec<u8> = Vec::with_capacity(total);
    while lengths.len() < total {
        let (value, repeat) = match code.decode(bits)? {
            symbol @ 0..=15 => (symbol as u8, 1),
            16 => (
                *lengths.last().ok_or(BAD_STREAM)?,
                3 + bits.take(2)? as usize,
            ),
            17 => (0, 3 + bits.take(3)? as usize),
            _ => (0, 11 + bits.take(7)? as usize),
        };
        if lengths.len() + repeat > total {
            return Err(BAD_STREAM);
        }
        lengths.resize(lengths.len() + repeat, value);
    }
    // A block without an end-of-block code cannot end.
    if lengths.get(256).is_none_or(|length| *length == 0) {
        return Err(BAD_STREAM);
    }
    let (literal, distance) = lengths.split_at_checked(literal_count).ok_or(BAD_STREAM)?;
    Ok((Huffman::new(literal)?, Huffman::new(distance)?))
}

/// Unpack a raw deflate stream. Fails with [`ZipError::TooLarge`] as soon as the output
/// would exceed `limit` bytes.
pub(crate) fn inflate(input: &[u8], limit: usize) -> Result<Vec<u8>, ZipError> {
    let mut bits = Bits {
        input,
        position: 0,
        buffer: 0,
        count: 0,
    };
    // Reserve what a well-formed stream of this size plausibly needs, not what it claims.
    let mut out = Vec::with_capacity(limit.min(input.len().saturating_mul(8)));
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => {
                bits.align();
                let length = bits.take(16)?;
                let complement = bits.take(16)?;
                if length != !complement & 0xFFFF {
                    return Err(BAD_STREAM);
                }
                let length = length as usize;
                if out.len() + length > limit {
                    return Err(ZipError::TooLarge);
                }
                bits.copy_bytes(length, &mut out)?;
            }
            1 => {
                let (literal, distance) = fixed_codes()?;
                inflate_block(&mut bits, &literal, &distance, &mut out, limit)?;
            }
            2 => {
                let (literal, distance) = dynamic_codes(&mut bits)?;
                inflate_block(&mut bits, &literal, &distance, &mut out, limit)?;
            }
            _ => return Err(BAD_STREAM),
        }
        if last {
            return Ok(out);
        }
    }
}

fn inflate_block(
    bits: &mut Bits<'_>,
    literal: &Huffman,
    distance: &Huffman,
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<(), ZipError> {
    loop {
        let symbol = literal.decode(bits)?;
        if symbol < 256 {
            if out.len() >= limit {
                return Err(ZipError::TooLarge);
            }
            out.push(symbol as u8);
            continue;
        }
        if symbol == 256 {
            return Ok(());
        }
        let at = symbol - 257;
        let base = LENGTH_BASE.get(at).copied().ok_or(BAD_STREAM)?;
        let extra = LENGTH_EXTRA.get(at).copied().ok_or(BAD_STREAM)?;
        let length = usize::from(base) + bits.take(u32::from(extra))? as usize;

        let at = distance.decode(bits)?;
        let base = DISTANCE_BASE.get(at).copied().ok_or(BAD_STREAM)?;
        let extra = DISTANCE_EXTRA.get(at).copied().ok_or(BAD_STREAM)?;
        let back = usize::from(base) + bits.take(u32::from(extra))? as usize;

        let start = out.len().checked_sub(back).ok_or(BAD_STREAM)?;
        if out.len() + length > limit {
            return Err(ZipError::TooLarge);
        }
        if back >= length {
            out.extend_from_within(start..start + length);
        } else {
            // The match overlaps what it produces: a short pattern repeated.
            for offset in 0..length {
                let byte = out.get(start + offset).copied().ok_or(BAD_STREAM)?;
                out.push(byte);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build an archive from `(name, method, flags, content as stored, unpacked content)`.
    pub(crate) struct Builder {
        bytes: Vec<u8>,
        directory: Vec<u8>,
        count: u16,
    }

    impl Builder {
        pub(crate) fn new() -> Self {
            Self {
                bytes: Vec::new(),
                directory: Vec::new(),
                count: 0,
            }
        }

        /// Add an entry whose content is kept as it is.
        pub(crate) fn stored(self, name: &str, content: &[u8]) -> Self {
            self.raw(
                name,
                METHOD_STORED,
                0,
                content,
                content.len(),
                crc32(content),
            )
        }

        /// Add an entry packed as one deflate "stored" block per 60,000 bytes.
        pub(crate) fn deflated(self, name: &str, content: &[u8]) -> Self {
            let packed = deflate_stored(content);
            self.raw(
                name,
                METHOD_DEFLATE,
                0,
                &packed,
                content.len(),
                crc32(content),
            )
        }

        pub(crate) fn raw(
            mut self,
            name: &str,
            method: u16,
            flags: u16,
            packed: &[u8],
            size: usize,
            crc: u32,
        ) -> Self {
            let offset = self.bytes.len() as u32;
            let mut common = Vec::new();
            common.extend(20u16.to_le_bytes()); // version needed
            common.extend(flags.to_le_bytes());
            common.extend(method.to_le_bytes());
            common.extend([0u8; 4]); // time, date
            common.extend(crc.to_le_bytes());
            common.extend((packed.len() as u32).to_le_bytes());
            common.extend((size as u32).to_le_bytes());
            common.extend((name.len() as u16).to_le_bytes());
            common.extend(0u16.to_le_bytes()); // extra length

            self.bytes.extend(LOCAL_SIGNATURE.to_le_bytes());
            self.bytes.extend(&common);
            self.bytes.extend(name.as_bytes());
            self.bytes.extend(packed);

            self.directory.extend(CENTRAL_SIGNATURE.to_le_bytes());
            self.directory.extend(20u16.to_le_bytes()); // version made by
            self.directory.extend(&common);
            self.directory.extend([0u8; 10]); // comment length, disk, attributes
            self.directory.extend(offset.to_le_bytes());
            self.directory.extend(name.as_bytes());
            self.count += 1;
            self
        }

        pub(crate) fn finish(mut self) -> Vec<u8> {
            let offset = self.bytes.len() as u32;
            let size = self.directory.len() as u32;
            self.bytes.extend(&self.directory);
            self.bytes.extend(END_SIGNATURE.to_le_bytes());
            self.bytes.extend([0u8; 4]); // disk numbers
            self.bytes.extend(self.count.to_le_bytes());
            self.bytes.extend(self.count.to_le_bytes());
            self.bytes.extend(size.to_le_bytes());
            self.bytes.extend(offset.to_le_bytes());
            self.bytes.extend(0u16.to_le_bytes()); // comment length
            self.bytes
        }
    }

    /// A deflate stream made of stored blocks only.
    pub(crate) fn deflate_stored(content: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunks = content.chunks(60_000).peekable();
        if chunks.peek().is_none() {
            return vec![0x01, 0x00, 0x00, 0xFF, 0xFF];
        }
        while let Some(chunk) = chunks.next() {
            out.push(u8::from(chunks.peek().is_none()));
            out.extend((chunk.len() as u16).to_le_bytes());
            out.extend((!(chunk.len() as u16)).to_le_bytes());
            out.extend(chunk);
        }
        out
    }

    /// Writes bits least significant first, as deflate packs them.
    struct BitWriter {
        out: Vec<u8>,
        buffer: u32,
        count: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                buffer: 0,
                count: 0,
            }
        }

        /// A plain number, least significant bit first.
        fn number(&mut self, value: u32, bits: u32) {
            for at in 0..bits {
                self.bit(value >> at & 1);
            }
        }

        /// A Huffman code, most significant bit first.
        fn code(&mut self, code: u32, bits: u32) {
            for at in (0..bits).rev() {
                self.bit(code >> at & 1);
            }
        }

        fn bit(&mut self, bit: u32) {
            self.buffer |= bit << self.count;
            self.count += 1;
            if self.count == 8 {
                self.out.push(self.buffer as u8);
                self.buffer = 0;
                self.count = 0;
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.count > 0 {
                self.out.push(self.buffer as u8);
            }
            self.out
        }
    }

    /// The fixed code of a literal/length symbol: (code, bits). RFC 1951 §3.2.6.
    fn fixed_literal(symbol: u32) -> (u32, u32) {
        match symbol {
            0..=143 => (0x30 + symbol, 8),
            144..=255 => (0x190 + symbol - 144, 9),
            256..=279 => (symbol - 256, 7),
            _ => (0xC0 + symbol - 280, 8),
        }
    }

    #[test]
    fn crc_check_value() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn stored_block() {
        assert_eq!(inflate(&deflate_stored(b"hello"), 100).unwrap(), b"hello");
        assert_eq!(inflate(&deflate_stored(b""), 100).unwrap(), b"");
        let long: Vec<u8> = (0..150_000u32).map(|n| (n * 7) as u8).collect();
        assert_eq!(inflate(&deflate_stored(&long), 150_000).unwrap(), long);
        assert_eq!(
            inflate(&deflate_stored(&long), 149_999),
            Err(ZipError::TooLarge)
        );
    }

    /// A fixed-code block written by hand: the literals "abc", then a match of length 6 at
    /// distance 3 (which overlaps its own output), then a match of length 4 at distance 9.
    #[test]
    fn fixed_block_with_matches() {
        let mut writer = BitWriter::new();
        writer.number(1, 1); // last block
        writer.number(1, 2); // fixed codes
        for byte in b"abc" {
            let (code, bits) = fixed_literal(u32::from(*byte));
            writer.code(code, bits);
        }
        // Length 6 is symbol 260 (no extra bits); distance 3 is symbol 2 (no extra bits).
        let (code, bits) = fixed_literal(260);
        writer.code(code, bits);
        writer.code(2, 5);
        // Length 4 is symbol 258; distance 9 is symbol 6 (base 9, 2 extra bits, value 0).
        let (code, bits) = fixed_literal(258);
        writer.code(code, bits);
        writer.code(6, 5);
        writer.number(0, 2);
        let (code, bits) = fixed_literal(256);
        writer.code(code, bits);
        let stream = writer.finish();
        assert_eq!(inflate(&stream, 100).unwrap(), b"abcabcabcabca");
        assert_eq!(inflate(&stream, 12), Err(ZipError::TooLarge));
        // Cut short anywhere, it is an error, never a panic.
        for cut in 0..stream.len() {
            assert!(inflate(&stream[..cut], 100).is_err(), "cut at {cut}");
        }
    }

    /// A dynamic block written by hand. The literal/length code has two symbols, 'a' and
    /// end-of-block, with one bit each; the code lengths are sent with runs of zeros (symbol
    /// 18), which is the part of the format the other tests do not reach.
    #[test]
    fn dynamic_block() {
        // Stored order of the code length code lengths, as far as 18 of them reach.
        let order = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1];
        let mut writer = BitWriter::new();
        writer.number(1, 1); // last block
        writer.number(2, 2); // dynamic codes
        writer.number(0, 5); // 257 literal/length codes
        writer.number(0, 5); // 1 distance code
        writer.number(14, 4); // 18 code length code lengths
        for symbol in order {
            let length = match symbol {
                0 | 1 => 2,
                18 => 1,
                _ => 0,
            };
            writer.number(length, 3);
        }
        // Canonical codes: 18 -> 0 (1 bit), 0 -> 10, 1 -> 11.
        let zeros = |writer: &mut BitWriter, count: u32| {
            writer.code(0b0, 1);
            writer.number(count - 11, 7);
        };
        zeros(&mut writer, 97);
        writer.code(0b11, 2); // 'a': length 1
        zeros(&mut writer, 138);
        zeros(&mut writer, 20);
        writer.code(0b11, 2); // end of block: length 1
        writer.code(0b10, 2); // the one distance code: unused
        // Data: 'a' is code 0, end of block is code 1.
        for _ in 0..5 {
            writer.code(0, 1);
        }
        writer.code(1, 1);
        let stream = writer.finish();
        assert_eq!(inflate(&stream, 100).unwrap(), b"aaaaa");
    }

    #[test]
    fn bad_streams_are_errors() {
        // Reserved block type.
        assert!(inflate(&[0x07], 10).is_err());
        // Stored block whose length and complement disagree.
        assert!(inflate(&[0x01, 0x05, 0x00, 0x00, 0x00, 1, 2, 3, 4, 5], 10).is_err());
        // Stored block that promises more than there is.
        assert!(inflate(&[0x01, 0x05, 0x00, 0xFA, 0xFF, 1, 2], 10).is_err());
        // A match that reaches back before the start of the output.
        let mut writer = BitWriter::new();
        writer.number(1, 1);
        writer.number(1, 2);
        let (code, bits) = fixed_literal(257);
        writer.code(code, bits);
        writer.code(0, 5);
        assert!(inflate(&writer.finish(), 10).is_err());
        // Empty input.
        assert!(inflate(&[], 10).is_err());
    }

    #[test]
    fn archive_with_stored_and_deflated_entries() {
        let text = "<a>å</a>".repeat(20_000);
        let bytes = Builder::new()
            .stored("P-0001/project.xml", b"<p/>")
            .deflated("P-0001/0.xml", text.as_bytes())
            .stored("M-00FA/Baggages/tool.dll", &[0u8; 64])
            .finish();
        let archive = Archive::open(&bytes).unwrap();
        let names: Vec<&str> = archive.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "P-0001/project.xml",
                "P-0001/0.xml",
                "M-00FA/Baggages/tool.dll"
            ]
        );
        let project = archive.entry("P-0001/project.xml").unwrap();
        assert_eq!(archive.read(project, 1000).unwrap(), b"<p/>");
        let zero = archive.entry("P-0001/0.xml").unwrap();
        assert_eq!(zero.size, text.len());
        assert_eq!(archive.read(zero, text.len()).unwrap(), text.as_bytes());
        assert_eq!(archive.read(zero, text.len() - 1), Err(ZipError::TooLarge));
        assert!(archive.entry("missing").is_none());
    }

    #[test]
    fn archive_with_a_comment_and_leading_bytes() {
        let mut bytes = Builder::new().stored("a.xml", b"<a/>").finish();
        // A comment after the end record, as some tools write.
        let at = bytes.len() - 2;
        bytes[at..].copy_from_slice(&5u16.to_le_bytes());
        bytes.extend(b"hello");
        let archive = Archive::open(&bytes).unwrap();
        assert_eq!(archive.read(&archive.entries()[0], 10).unwrap(), b"<a/>");
    }

    #[test]
    fn damaged_archives_are_errors() {
        let good = Builder::new()
            .stored("a.xml", b"<a/>")
            .deflated("b.xml", b"<b/>")
            .finish();
        assert!(Archive::open(&[]).is_err());
        assert!(Archive::open(b"not a zip at all, just text").is_err());
        // Every truncation fails to open or fails to read, without panicking.
        for cut in 0..good.len() {
            if let Ok(archive) = Archive::open(&good[..cut]) {
                for entry in archive.entries() {
                    let _ = archive.read(entry, 1000);
                }
            }
        }
        // A flipped content byte is caught by the checksum.
        let mut flipped = good.clone();
        let at = flipped.iter().position(|b| *b == b'<').unwrap();
        flipped[at] = b'[';
        let archive = Archive::open(&flipped).unwrap();
        assert_eq!(
            archive.read(&archive.entries()[0], 1000),
            Err(ZipError::Corrupt)
        );
        // A directory size that lies about the content.
        let lying = Builder::new()
            .raw("a.xml", METHOD_STORED, 0, b"<a/>", 9, crc32(b"<a/>"))
            .finish();
        let archive = Archive::open(&lying).unwrap();
        assert_eq!(
            archive.read(&archive.entries()[0], 1000),
            Err(ZipError::Corrupt)
        );
    }

    #[test]
    fn encrypted_and_unsupported_entries_are_refused() {
        let bytes = Builder::new()
            .raw("classic.xml", METHOD_STORED, FLAG_ENCRYPTED, b"????", 4, 0)
            .raw("aes.xml", METHOD_AES, FLAG_ENCRYPTED, b"????", 4, 0)
            .raw("lzma.xml", 14, 0, b"????", 4, 0)
            .finish();
        let archive = Archive::open(&bytes).unwrap();
        let [classic, aes, lzma] = archive.entries() else {
            panic!("three entries")
        };
        assert!(classic.is_encrypted() && aes.is_encrypted() && !lzma.is_encrypted());
        assert_eq!(archive.read(classic, 100), Err(ZipError::Encrypted));
        assert_eq!(archive.read(aes, 100), Err(ZipError::Encrypted));
        assert_eq!(
            archive.read(lzma, 100),
            Err(ZipError::Unsupported("compression method"))
        );
    }

    #[test]
    fn zip64_and_oversized_directories_are_refused() {
        let mut bytes = Builder::new().stored("a.xml", b"<a/>").finish();
        let end = bytes.len() - END_SIZE;
        // Entry counts of 0xFFFF mark ZIP64.
        bytes[end + 8..end + 12].copy_from_slice(&[0xFF; 4]);
        assert_eq!(
            Archive::open(&bytes).unwrap_err(),
            ZipError::Unsupported("ZIP64")
        );
        // More entries than the limit.
        let count = (MAX_ENTRIES as u16 + 1).to_le_bytes();
        bytes[end + 8..end + 10].copy_from_slice(&count);
        bytes[end + 10..end + 12].copy_from_slice(&count);
        assert_eq!(Archive::open(&bytes).unwrap_err(), ZipError::TooLarge);
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn inflate_survives_any_bytes(
                bytes in proptest::collection::vec(any::<u8>(), 0..200),
                limit in 0usize..2000,
            ) {
                if let Ok(out) = inflate(&bytes, limit) {
                    prop_assert!(out.len() <= limit);
                }
            }

            /// Streams that start like a fixed or dynamic block get further into the decoder.
            #[test]
            fn inflate_survives_block_soup(
                header in prop_oneof![Just(0x03u8), Just(0x05u8), Just(0x04u8), Just(0x02u8)],
                bytes in proptest::collection::vec(any::<u8>(), 0..200),
            ) {
                let mut stream = vec![header];
                stream.extend(bytes);
                if let Ok(out) = inflate(&stream, 4096) {
                    prop_assert!(out.len() <= 4096);
                }
            }

            #[test]
            fn stored_blocks_round_trip(content in proptest::collection::vec(any::<u8>(), 0..5000)) {
                let packed = deflate_stored(&content);
                prop_assert_eq!(inflate(&packed, content.len()).unwrap(), content);
            }

            #[test]
            fn archives_round_trip(
                first in proptest::collection::vec(any::<u8>(), 0..400),
                second in proptest::collection::vec(any::<u8>(), 0..400),
            ) {
                let bytes = Builder::new()
                    .stored("a/first.xml", &first)
                    .deflated("b/second.xml", &second)
                    .finish();
                let archive = Archive::open(&bytes).unwrap();
                let read = |name: &str| archive.read(archive.entry(name).unwrap(), 400).unwrap();
                prop_assert_eq!(read("a/first.xml"), first);
                prop_assert_eq!(read("b/second.xml"), second);
            }

            #[test]
            fn archives_survive_any_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
                if let Ok(archive) = Archive::open(&bytes) {
                    for entry in archive.entries() {
                        let _ = archive.read(entry, 1000);
                    }
                }
            }

            /// A valid archive with some bytes overwritten: errors at worst.
            #[test]
            fn damaged_archives_survive(
                content in proptest::collection::vec(any::<u8>(), 0..200),
                damage in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
            ) {
                let mut bytes = Builder::new()
                    .stored("P-0001/0.xml", &content)
                    .deflated("M-00FA/Hardware.xml", &content)
                    .finish();
                for (at, value) in damage {
                    let at = at.index(bytes.len());
                    bytes[at] = value;
                }
                if let Ok(archive) = Archive::open(&bytes) {
                    for entry in archive.entries() {
                        if let Ok(read) = archive.read(entry, 1000) {
                            prop_assert!(read.len() <= 1000);
                        }
                    }
                }
            }
        }
    }
}
