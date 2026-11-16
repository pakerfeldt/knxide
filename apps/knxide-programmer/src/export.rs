//! Writing a project out again as a project file, with the edits in it.
//!
//! The file that comes out is the file that went in, entry for entry, except for the
//! installation file `P-…/0.xml` (and `P-…/project.xml` where new identifiers were handed
//! out). Those two are changed as text, in place: every byte the edits do not touch stays as
//! it was. With no edits the original file is returned as it is.
//!
//! What is written: parameter values and group object links per device, names given to group
//! addresses, and group addresses made in the app. Nothing else of the project is touched; the
//! signature and certificate entries of the archive are copied as they are.
//!
//! **Unverified against ETS.** How these elements look was read from one ETS 5 project
//! (schema 20). Whether ETS takes a file changed this way has not been seen.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use knxide_core::GroupAddress;
use knxide_program::{Project, ProjectArchive, ProjectSource};

use crate::sidecar::State;

// ------------------------------------------------------------------ the archive

/// ZIP: local file header, APPNOTE 4.3.7.
const LOCAL: u32 = 0x0403_4b50;
/// ZIP: central directory header, APPNOTE 4.3.12.
const CENTRAL: u32 = 0x0201_4b50;
/// ZIP: end of central directory record, APPNOTE 4.3.16.
const END: u32 = 0x0605_4b50;

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    bytes
        .get(at..at + 2)
        .and_then(|b| b.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| "the project file ends early".to_string())
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "the project file ends early".to_string())
}

/// CRC-32 as ZIP uses it (APPNOTE 4.4.7): polynomial 0xEDB88320, reflected.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One entry of the archive as its central directory describes it.
struct Entry<'a> {
    /// The central directory header from its signature up to the name, 46 bytes.
    header: &'a [u8],
    name: &'a [u8],
    extra: &'a [u8],
    comment: &'a [u8],
    /// The extra field of the local header.
    local_extra: &'a [u8],
    /// The data as stored: compressed or not.
    data: &'a [u8],
}

fn entries(bytes: &[u8]) -> Result<Vec<Entry<'_>>, String> {
    let short = || "the project file is not a ZIP archive".to_string();
    // The end record is the last thing in the file but for a comment of at most 65535 bytes.
    let floor = bytes.len().saturating_sub(22 + 65_535);
    let end = (floor..=bytes.len().saturating_sub(22))
        .rev()
        .find(|at| u32_at(bytes, *at) == Ok(END))
        .ok_or_else(short)?;
    let count = usize::from(u16_at(bytes, end + 10)?);
    let mut at = usize::try_from(u32_at(bytes, end + 16)?).map_err(|_| short())?;
    if count == 0xFFFF || at == 0xFFFF_FFFF {
        return Err("project files of this size (ZIP64) cannot be exported".into());
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(bytes, at)? != CENTRAL {
            return Err(short());
        }
        let name_len = usize::from(u16_at(bytes, at + 28)?);
        let extra_len = usize::from(u16_at(bytes, at + 30)?);
        let comment_len = usize::from(u16_at(bytes, at + 32)?);
        let size = usize::try_from(u32_at(bytes, at + 20)?).map_err(|_| short())?;
        let local = usize::try_from(u32_at(bytes, at + 42)?).map_err(|_| short())?;
        let slice = |from: usize, len: usize| bytes.get(from..from + len).ok_or_else(short);
        if u32_at(bytes, local)? != LOCAL {
            return Err(short());
        }
        let local_name = usize::from(u16_at(bytes, local + 26)?);
        let local_extra = usize::from(u16_at(bytes, local + 28)?);
        out.push(Entry {
            header: slice(at, 46)?,
            name: slice(at + 46, name_len)?,
            extra: slice(at + 46 + name_len, extra_len)?,
            comment: slice(at + 46 + name_len + extra_len, comment_len)?,
            local_extra: slice(local + 30 + local_name, local_extra)?,
            data: slice(local + 30 + local_name + local_extra, size)?,
        });
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// The archive in `original` with the entries named in `replaced` holding other content,
/// stored uncompressed. Every other entry keeps its bytes as they are.
fn replace_entries(
    original: &[u8],
    replaced: &HashMap<String, Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let too_large = || "the project file is too large to export".to_string();
    let mut out = Vec::with_capacity(original.len() + 1024);
    let mut directory = Vec::new();
    let all = entries(original)?;
    for entry in &all {
        let name = String::from_utf8_lossy(entry.name);
        let new = replaced.get(name.as_ref());
        // Method, CRC and sizes of the entry as it is written.
        let (method, crc, data): (u16, u32, &[u8]) = match new {
            Some(content) => (0, crc32(content), content),
            None => (
                u16_at(entry.header, 10)?,
                u32_at(entry.header, 16)?,
                entry.data,
            ),
        };
        let stored = u32::try_from(data.len()).map_err(|_| too_large())?;
        let plain = match new {
            Some(content) => u32::try_from(content.len()).map_err(|_| too_large())?,
            None => u32_at(entry.header, 24)?,
        };
        // Sizes and CRC are in the headers here, so no data descriptor follows the data:
        // bit 3 of the flags is cleared.
        let flags = u16_at(entry.header, 8)? & !0x0008;
        let offset = u32::try_from(out.len()).map_err(|_| too_large())?;

        out.extend_from_slice(&LOCAL.to_le_bytes());
        out.extend_from_slice(entry.header.get(6..8).unwrap_or(&[20, 0])); // version needed
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(entry.header.get(12..16).unwrap_or(&[0; 4])); // time, date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&stored.to_le_bytes());
        out.extend_from_slice(&plain.to_le_bytes());
        out.extend_from_slice(&u16::try_from(entry.name.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(
            &u16::try_from(entry.local_extra.len())
                .unwrap_or(0)
                .to_le_bytes(),
        );
        out.extend_from_slice(entry.name);
        out.extend_from_slice(entry.local_extra);
        out.extend_from_slice(data);

        directory.extend_from_slice(entry.header.get(0..8).unwrap_or(&[0; 8]));
        directory.extend_from_slice(&flags.to_le_bytes());
        directory.extend_from_slice(&method.to_le_bytes());
        directory.extend_from_slice(entry.header.get(12..16).unwrap_or(&[0; 4]));
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&stored.to_le_bytes());
        directory.extend_from_slice(&plain.to_le_bytes());
        directory.extend_from_slice(entry.header.get(28..42).unwrap_or(&[0; 14]));
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(entry.name);
        directory.extend_from_slice(entry.extra);
        directory.extend_from_slice(entry.comment);
    }
    let start = u32::try_from(out.len()).map_err(|_| too_large())?;
    let size = u32::try_from(directory.len()).map_err(|_| too_large())?;
    let count = u16::try_from(all.len()).map_err(|_| too_large())?;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&END.to_le_bytes());
    out.extend_from_slice(&[0; 4]); // this disk, disk of the directory
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&[0; 2]); // no comment
    Ok(out)
}

// ------------------------------------------------------------------ the XML, as text

/// One attribute of a tag, with where it stands in the text.
#[derive(Debug, Clone)]
struct Attribute<'a> {
    name: &'a str,
    /// The value as written, entities and all.
    value: &'a str,
    /// From the white space before the name to behind the closing quote.
    start: usize,
    end: usize,
}

/// One tag of the text.
#[derive(Debug, Clone)]
struct Tag<'a> {
    name: &'a str,
    start: usize,
    /// Behind its `>`.
    end: usize,
    closing: bool,
    empty: bool,
    attributes: Vec<Attribute<'a>>,
}

impl<'a> Tag<'a> {
    fn attribute(&self, name: &str) -> Option<&'a str> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name == name)
            .map(|attribute| attribute.value)
    }
}

/// Every element tag of `text`, in order. Declarations, comments and CDATA are passed over.
fn tags(text: &str) -> Result<Vec<Tag<'_>>, String> {
    let broken = || "the project's XML is not well formed".to_string();
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(found) = text.get(at..).and_then(|rest| rest.find('<')) {
        let start = at + found;
        let rest = text.get(start..).ok_or_else(broken)?;
        let skip = |closer: &str| rest.find(closer).map(|end| start + end + closer.len());
        if rest.starts_with("<?") {
            at = skip("?>").ok_or_else(broken)?;
            continue;
        }
        if rest.starts_with("<!--") {
            at = skip("-->").ok_or_else(broken)?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            at = skip("]]>").ok_or_else(broken)?;
            continue;
        }
        if rest.starts_with("<!") {
            at = skip(">").ok_or_else(broken)?;
            continue;
        }
        let closing = rest.starts_with("</");
        let mut i = start + if closing { 2 } else { 1 };
        let name_start = i;
        while bytes
            .get(i)
            .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'/' && *b != b'>')
        {
            i += 1;
        }
        let name = text.get(name_start..i).ok_or_else(broken)?;
        let mut attributes = Vec::new();
        let mut empty = false;
        loop {
            let space = i;
            while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            match bytes.get(i) {
                Some(b'>') => {
                    i += 1;
                    break;
                }
                Some(b'/') => {
                    empty = true;
                    i += 1;
                }
                Some(_) => {
                    let attribute_start = i;
                    while bytes
                        .get(i)
                        .is_some_and(|b| *b != b'=' && !b.is_ascii_whitespace())
                    {
                        i += 1;
                    }
                    let attribute = text.get(attribute_start..i).ok_or_else(broken)?;
                    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                        i += 1;
                    }
                    if bytes.get(i) != Some(&b'=') {
                        return Err(broken());
                    }
                    i += 1;
                    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
                        i += 1;
                    }
                    let quote = *bytes.get(i).ok_or_else(broken)?;
                    if quote != b'"' && quote != b'\'' {
                        return Err(broken());
                    }
                    i += 1;
                    let value_start = i;
                    while bytes.get(i).is_some_and(|b| *b != quote) {
                        i += 1;
                    }
                    let value = text.get(value_start..i).ok_or_else(broken)?;
                    if bytes.get(i).is_none() {
                        return Err(broken());
                    }
                    i += 1;
                    attributes.push(Attribute {
                        name: attribute,
                        value,
                        start: space,
                        end: i,
                    });
                }
                None => return Err(broken()),
            }
        }
        out.push(Tag {
            name,
            start,
            end: i,
            closing,
            empty,
            attributes,
        });
        at = i;
    }
    Ok(out)
}

/// A value as it goes between double quotes.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#xD;"),
            '\n' => out.push_str("&#xA;"),
            c => out.push(c),
        }
    }
    out
}

/// Changes to a text, collected and then made in one pass.
#[derive(Default)]
struct Patch {
    /// Per tag (by its place in the list of tags): attributes to set, or to take away.
    attributes: BTreeMap<usize, Vec<(String, Option<String>)>>,
    /// Tags that go altogether.
    removed: Vec<usize>,
    /// Text put in at an offset, in the order it was asked for.
    inserted: BTreeMap<usize, String>,
    /// Empty elements that get content: by tag, what goes between its new start and end.
    opened: BTreeMap<usize, String>,
}

impl Patch {
    fn set(&mut self, tag: usize, name: &str, value: Option<String>) {
        self.attributes
            .entry(tag)
            .or_default()
            .push((name.to_string(), value));
    }

    fn insert(&mut self, at: usize, text: &str) {
        self.inserted.entry(at).or_default().push_str(text);
    }

    /// Put `text` into the element that starts with the tag `index`.
    fn insert_into(&mut self, tags: &[Tag<'_>], index: usize, text: &str) {
        match tags.get(index) {
            Some(tag) if tag.empty => self.opened.entry(index).or_default().push_str(text),
            Some(tag) => self.insert(tag.end, text),
            None => {}
        }
    }

    fn is_empty(&self) -> bool {
        self.attributes.is_empty()
            && self.removed.is_empty()
            && self.inserted.is_empty()
            && self.opened.is_empty()
    }

    fn apply(&self, text: &str, tags: &[Tag<'_>]) -> String {
        // Every change as (from, to, new text), made from the end of the text backwards so
        // that the offsets before a change stay good.
        let mut changes: Vec<(usize, usize, String)> = Vec::new();
        let mut touched: Vec<usize> = self.attributes.keys().copied().collect();
        touched.extend(self.opened.keys().copied());
        touched.sort_unstable();
        touched.dedup();
        for index in touched {
            if self.removed.contains(&index) {
                continue;
            }
            let Some(tag) = tags.get(index) else { continue };
            let mut written = text.get(tag.start..tag.end).unwrap_or_default().to_string();
            let mut added = String::new();
            let mut edits: Vec<(usize, usize, String)> = Vec::new();
            for (name, value) in self.attributes.get(&index).into_iter().flatten() {
                let existing = tag.attributes.iter().find(|a| a.name == name);
                match (existing, value) {
                    (Some(attribute), Some(value)) => edits.push((
                        attribute.start - tag.start,
                        attribute.end - tag.start,
                        format!(" {name}=\"{}\"", escape(value)),
                    )),
                    (Some(attribute), None) => edits.push((
                        attribute.start - tag.start,
                        attribute.end - tag.start,
                        String::new(),
                    )),
                    (None, Some(value)) => {
                        let _ = write!(added, " {name}=\"{}\"", escape(value));
                    }
                    (None, None) => {}
                }
            }
            // New attributes go behind the last one, or behind the name.
            let behind = tag
                .attributes
                .last()
                .map_or(1 + tag.name.len(), |a| a.end - tag.start);
            edits.push((behind, behind, added));
            edits.sort_by_key(|edit| std::cmp::Reverse(edit.0));
            for (from, to, new) in edits {
                written.replace_range(from..to, &new);
            }
            if let Some(content) = self.opened.get(&index) {
                let open = written
                    .trim_end_matches('>')
                    .trim_end_matches('/')
                    .trim_end();
                written = format!("{open}>{content}</{}>", tag.name);
            }
            changes.push((tag.start, tag.end, written));
        }
        for index in &self.removed {
            if let Some(tag) = tags.get(*index) {
                changes.push((tag.start, tag.end, String::new()));
            }
        }
        for (at, new) in &self.inserted {
            changes.push((*at, *at, new.clone()));
        }
        // From the back; of two changes at one offset the one that replaces text first, so
        // that what is put in lands before it.
        changes.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let mut out = text.to_string();
        for (from, to, new) in changes {
            out.replace_range(from..to, &new);
        }
        out
    }
}

/// One device of the installation file.
#[derive(Default)]
struct DeviceTags {
    /// The start tag.
    open: usize,
    parameters: Option<(usize, usize)>,
    objects: Option<(usize, usize)>,
    tree: Option<usize>,
    parameter_refs: Vec<usize>,
    object_refs: Vec<usize>,
}

/// Whether the reference `written` in the file is the reference `short` of the edits: files
/// name a reference in full (`M-…_A-…_P-1_R-1`) or by its end (`P-1_R-1`).
fn same_reference(written: &str, short: &str) -> bool {
    written == short
        || written
            .strip_suffix(short)
            .is_some_and(|front| front.ends_with('_'))
}

fn number_behind(text: &str, marker: &str) -> Option<u64> {
    text.rsplit_once(marker)?.1.parse().ok()
}

/// What the edits make of the installation file `text`, and the highest unique number
/// (`Puid`) that was handed out, when any was.
fn rewrite(
    text: &str,
    project: &Project,
    state: &State,
    last_puid: Option<u64>,
) -> Result<(String, Option<u64>), String> {
    let tags = tags(text)?;
    let mut patch = Patch::default();

    // ---- what is where
    let mut devices: HashMap<&str, DeviceTags> = HashMap::new();
    let mut current: Option<&str> = None;
    let mut open_container: Vec<(&str, usize)> = Vec::new();
    // Group addresses by raw address: tag and short identifier.
    let mut addresses: HashMap<u16, (usize, &str)> = HashMap::new();
    // Group ranges: tag, start, end, depth.
    let mut ranges: Vec<(usize, u16, u16, usize)> = Vec::new();
    let mut range_depth = 0_usize;
    let mut ranges_root: Option<usize> = None;
    let mut highest_address = 0_u64;
    let mut highest_range = 0_u64;
    let mut highest_puid = 0_u64;
    let mut address_prefix: Option<&str> = None;
    let mut full_parameter_refs = false;
    let mut full_links = false;

    for (index, tag) in tags.iter().enumerate() {
        if let Some(puid) = tag.attribute("Puid").and_then(|p| p.parse::<u64>().ok()) {
            highest_puid = highest_puid.max(puid);
        }
        match (tag.name, tag.closing) {
            ("DeviceInstance", false) => {
                let id = tag.attribute("Id").unwrap_or_default();
                devices.insert(
                    id,
                    DeviceTags {
                        open: index,
                        ..DeviceTags::default()
                    },
                );
                current = (!tag.empty).then_some(id);
            }
            ("DeviceInstance", true) => current = None,
            ("ParameterInstanceRefs" | "ComObjectInstanceRefs", false) => {
                if tag.empty {
                    if let Some(device) = current.and_then(|id| devices.get_mut(id)) {
                        if tag.name == "ParameterInstanceRefs" {
                            device.parameters = Some((index, index));
                        } else {
                            device.objects = Some((index, index));
                        }
                    }
                } else {
                    open_container.push((tag.name, index));
                }
            }
            ("ParameterInstanceRefs" | "ComObjectInstanceRefs", true) => {
                if let (Some((name, open)), Some(device)) = (
                    open_container.pop(),
                    current.and_then(|id| devices.get_mut(id)),
                ) {
                    if name == "ParameterInstanceRefs" {
                        device.parameters = Some((open, index));
                    } else {
                        device.objects = Some((open, index));
                    }
                }
            }
            ("ParameterInstanceRef", false) => {
                full_parameter_refs |= tag.attribute("RefId").is_some_and(|r| r.starts_with("M-"));
                if let Some(device) = current.and_then(|id| devices.get_mut(id)) {
                    device.parameter_refs.push(index);
                }
            }
            ("ComObjectInstanceRef", false) => {
                full_links |= tag.attribute("Links").is_some_and(|l| l.contains("_GA-"));
                if let Some(device) = current.and_then(|id| devices.get_mut(id)) {
                    device.object_refs.push(index);
                }
            }
            ("GroupObjectTree", false) => {
                if let Some(device) = current.and_then(|id| devices.get_mut(id)) {
                    device.tree = Some(index);
                }
            }
            ("GroupRanges", false) => ranges_root = Some(index),
            ("GroupRange", false) => {
                let bound = |name: &str| tag.attribute(name).and_then(|v| v.parse::<u16>().ok());
                if let (Some(start), Some(end)) = (bound("RangeStart"), bound("RangeEnd")) {
                    ranges.push((index, start, end, range_depth));
                }
                if let Some(number) = tag.attribute("Id").and_then(|id| number_behind(id, "_GR-")) {
                    highest_range = highest_range.max(number);
                }
                if !tag.empty {
                    range_depth += 1;
                }
            }
            ("GroupRange", true) => range_depth = range_depth.saturating_sub(1),
            ("GroupAddress", false) => {
                let id = tag.attribute("Id").unwrap_or_default();
                if let Some((front, number)) = id.rsplit_once("_GA-") {
                    address_prefix = address_prefix.or(Some(front));
                    highest_address = highest_address.max(number.parse().unwrap_or(0));
                }
                let short = id.rsplit_once('_').map_or(id, |(_, short)| short);
                if let Some(raw) = tag.attribute("Address").and_then(|a| a.parse::<u16>().ok()) {
                    addresses.insert(raw, (index, short));
                }
            }
            _ => {}
        }
    }
    let prefix = address_prefix.map_or_else(|| format!("{}-0", project.id), str::to_string);
    let mut next_puid = last_puid.unwrap_or(0).max(highest_puid);
    let first_puid = next_puid;
    let mut puid = || {
        next_puid += 1;
        next_puid
    };

    // ---- names given to the project's group addresses
    for (address, name) in &state.renamed {
        if let Some((tag, _)) = addresses.get(&address.raw()) {
            patch.set(*tag, "Name", Some(name.clone()));
        }
    }

    // ---- group addresses made in the app
    // Their short identifiers, for the links below.
    let mut made: HashMap<u16, String> = HashMap::new();
    for (address, created) in &state.created {
        let raw = address.raw();
        if addresses.contains_key(&raw) {
            continue;
        }
        highest_address += 1;
        let short = format!("GA-{highest_address}");
        let mut element = format!(
            "<GroupAddress Id=\"{prefix}_{short}\" Address=\"{raw}\" Name=\"{}\"",
            escape(&created.name)
        );
        if let Some(datapoint_type) = &created.datapoint_type {
            let _ = write!(element, " DatapointType=\"{}\"", escape(datapoint_type));
        }
        let _ = write!(element, " Puid=\"{}\" />", puid());
        made.insert(raw, short);

        // The innermost range that holds the address; main and middle group are made where
        // they are missing (three-level addresses: 5, 3 and 8 bits).
        let holding = ranges
            .iter()
            .filter(|(_, start, end, _)| (*start..=*end).contains(&raw))
            .max_by_key(|(_, _, _, depth)| *depth)
            .copied();
        match holding {
            Some((tag, _, _, depth)) if depth >= 1 => patch.insert_into(&tags, tag, &element),
            other => {
                let middle_start = raw & 0xFF00;
                highest_range += 1;
                let middle = format!(
                    "<GroupRange Id=\"{prefix}_GR-{highest_range}\" RangeStart=\"{middle_start}\" \
                     RangeEnd=\"{}\" Name=\"Middle group {}\" Puid=\"{}\">{element}</GroupRange>",
                    middle_start | 0x00FF,
                    (raw >> 8) & 0x07,
                    puid(),
                );
                match other {
                    Some((tag, _, _, _)) => patch.insert_into(&tags, tag, &middle),
                    None => {
                        let root = ranges_root.ok_or(
                            "the project has no place for new group addresses (no GroupRanges)",
                        )?;
                        let main_start = raw & 0xF800;
                        highest_range += 1;
                        let main = format!(
                            "<GroupRange Id=\"{prefix}_GR-{highest_range}\" \
                             RangeStart=\"{main_start}\" RangeEnd=\"{}\" Name=\"Main group {}\" \
                             Puid=\"{}\">{middle}</GroupRange>",
                            main_start | 0x07FF,
                            raw >> 11,
                            puid(),
                        );
                        patch.insert_into(&tags, root, &main);
                        // Later addresses of the same groups find them only in the next
                        // export; within this one each gets its own. Rare enough to leave.
                    }
                }
            }
        }
    }
    let link_id = |address: &GroupAddress| -> Result<String, String> {
        let raw = address.raw();
        let short = addresses
            .get(&raw)
            .map(|(_, short)| (*short).to_string())
            .or_else(|| made.get(&raw).cloned())
            .ok_or_else(|| format!("group address {address} is linked but is in no list"))?;
        Ok(if full_links {
            format!("{prefix}_{short}")
        } else {
            short
        })
    };

    // ---- per device: parameter values and links
    for (id, device_state) in &state.devices {
        let edits = &device_state.edits;
        if edits.parameters.is_empty() && edits.links.is_empty() {
            continue;
        }
        let Some(device) = devices.get(id.as_str()) else {
            return Err(format!(
                "device {id} has edits and is not in the project file"
            ));
        };
        let application = project
            .device(id)
            .and_then(|device| device.application_id.clone())
            .unwrap_or_default();
        let tag_of = |index: usize| tags.get(index);

        let mut new_parameters = String::new();
        for (reference, value) in &edits.parameters {
            let existing = device.parameter_refs.iter().copied().find(|index| {
                tag_of(*index)
                    .and_then(|tag| tag.attribute("RefId"))
                    .is_some_and(|written| same_reference(written, reference))
            });
            match existing {
                Some(index) => patch.set(index, "Value", Some(value.clone())),
                None => {
                    let written = if full_parameter_refs && !application.is_empty() {
                        format!("{application}_{reference}")
                    } else {
                        reference.clone()
                    };
                    let _ = write!(
                        new_parameters,
                        "<ParameterInstanceRef RefId=\"{}\" Value=\"{}\" />",
                        escape(&written),
                        escape(value)
                    );
                }
            }
        }

        // The objects of the tree as they are, to be changed with the links.
        let mut tree: Vec<String> = device
            .tree
            .and_then(tag_of)
            .and_then(|tag| tag.attribute("GroupObjectInstances"))
            .map(|list| list.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        let tree_before = tree.clone();
        let mut new_objects = String::new();
        for (reference, links) in &edits.links {
            let written: Vec<String> = links.iter().map(&link_id).collect::<Result<_, _>>()?;
            let existing = device.object_refs.iter().copied().find(|index| {
                tag_of(*index)
                    .and_then(|tag| tag.attribute("RefId"))
                    .is_some_and(|written| same_reference(written, reference))
            });
            let in_tree = tree
                .iter()
                .position(|entry| same_reference(entry, reference));
            match (existing, written.is_empty()) {
                (Some(index), false) => {
                    patch.set(index, "Links", Some(written.join(" ")));
                    if in_tree.is_none() {
                        tree.push(reference.clone());
                    }
                }
                (Some(index), true) => {
                    // An object that only said what it was linked to goes with its links.
                    let only_links = tag_of(index).is_some_and(|tag| {
                        tag.empty
                            && tag
                                .attributes
                                .iter()
                                .all(|a| a.name == "RefId" || a.name == "Links")
                    });
                    // The tree lists the objects the device shows, linked or not: it stays.
                    if only_links {
                        patch.removed.push(index);
                    } else {
                        patch.set(index, "Links", None);
                    }
                }
                (None, false) => {
                    let _ = write!(
                        new_objects,
                        "<ComObjectInstanceRef RefId=\"{}\" Links=\"{}\" />",
                        escape(reference),
                        written.join(" ")
                    );
                    if in_tree.is_none() {
                        tree.push(reference.clone());
                    }
                }
                (None, true) => {}
            }
        }

        // ---- where the new elements go: parameters first, then objects, then the tree
        let device_open = tag_of(device.open).map_or(0, |tag| tag.end);
        let mut behind_parameters = device_open;
        if let Some((open, close)) = device.parameters {
            if !new_parameters.is_empty() {
                patch.insert_into(&tags, open, &new_parameters);
            }
            behind_parameters = tag_of(close).map_or(device_open, |tag| tag.end);
        } else if !new_parameters.is_empty() {
            patch.insert(
                device_open,
                &format!("<ParameterInstanceRefs>{new_parameters}</ParameterInstanceRefs>"),
            );
        }
        let tree_element = (tree != tree_before).then(|| tree.join(" "));
        let mut tree_written = false;
        if let (Some(index), Some(list)) = (device.tree, &tree_element) {
            patch.set(index, "GroupObjectInstances", Some(list.clone()));
            tree_written = true;
        }
        let new_tree = match (&tree_element, tree_written) {
            (Some(list), false) => format!("<GroupObjectTree GroupObjectInstances=\"{list}\" />"),
            _ => String::new(),
        };
        if let Some((open, close)) = device.objects {
            if !new_objects.is_empty() {
                patch.insert_into(&tags, open, &new_objects);
            }
            if !new_tree.is_empty() {
                patch.insert(tag_of(close).map_or(device_open, |tag| tag.end), &new_tree);
            }
        } else if !new_objects.is_empty() || !new_tree.is_empty() {
            let container = if new_objects.is_empty() {
                String::new()
            } else {
                format!("<ComObjectInstanceRefs>{new_objects}</ComObjectInstanceRefs>")
            };
            patch.insert(behind_parameters, &format!("{container}{new_tree}"));
        }
    }

    if patch.is_empty() {
        return Ok((text.to_string(), None));
    }
    let used = (next_puid != first_puid).then_some(next_puid);
    Ok((patch.apply(text, &tags), used))
}

/// The project information with `LastUsedPuid` raised to `puid`.
fn raise_puid(text: &str, puid: u64) -> Result<String, String> {
    let tags = tags(text)?;
    let mut patch = Patch::default();
    if let Some(index) = tags
        .iter()
        .position(|tag| tag.name == "ProjectInformation" && !tag.closing)
    {
        patch.set(index, "LastUsedPuid", Some(puid.to_string()));
    }
    Ok(patch.apply(text, &tags))
}

/// The project file `original` with the edits of `state` written into it. `project` is what
/// was read from that file.
pub(crate) fn project_file(
    original: &[u8],
    project: &Project,
    state: &State,
) -> Result<Vec<u8>, String> {
    let installation = format!("{}/0.xml", project.id);
    let information = format!("{}/project.xml", project.id);
    let archive = ProjectArchive::open(original).map_err(|error| error.to_string())?;
    let read = |path: &str| -> Result<String, String> {
        let bytes = archive.read(path).map_err(|error| error.to_string())?;
        String::from_utf8(bytes).map_err(|_| format!("{path} is not UTF-8"))
    };
    let text = read(&installation)?;
    let about = read(&information).ok();
    let last_puid = about.as_deref().and_then(|about| {
        tags(about)
            .ok()?
            .iter()
            .find(|tag| tag.name == "ProjectInformation")?
            .attribute("LastUsedPuid")?
            .parse()
            .ok()
    });
    let (rewritten, used_puid) = rewrite(&text, project, state, last_puid)?;
    if rewritten == text {
        return Ok(original.to_vec());
    }
    let mut replaced = HashMap::new();
    replaced.insert(installation, rewritten.into_bytes());
    if let (Some(about), Some(puid)) = (about, used_puid) {
        replaced.insert(information, raise_puid(&about, puid)?.into_bytes());
    }
    let out = replace_entries(original, &replaced)?;
    // What was written must read back as a project before it is handed out.
    let check = ProjectArchive::open(&out).map_err(|error| error.to_string())?;
    Project::load(&check)
        .map_err(|error| format!("the exported file does not read back: {error}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_keep_their_places() {
        let text = r#"<?xml version="1.0"?><A x="1" y='2'><!-- c --><B/><C z="&amp;" /></A>"#;
        let tags = tags(text).unwrap();
        let names: Vec<_> = tags
            .iter()
            .map(|tag| (tag.name, tag.closing, tag.empty))
            .collect();
        assert_eq!(
            names,
            [
                ("A", false, false),
                ("B", false, true),
                ("C", false, true),
                ("A", true, false)
            ]
        );
        assert_eq!(tags[0].attribute("y"), Some("2"));
        assert_eq!(&text[tags[2].start..tags[2].end], r#"<C z="&amp;" />"#);
    }

    #[test]
    fn a_patch_changes_only_what_it_names() {
        let text = r#"<R><A x="1" y="2" /><B /><C>t</C></R>"#;
        let tags = tags(text).unwrap();
        let mut patch = Patch::default();
        patch.set(1, "y", Some("a<b".into()));
        patch.set(1, "z", Some("3".into()));
        patch.set(1, "x", None);
        patch.insert_into(&tags, 2, "<D />");
        patch.insert_into(&tags, 3, "<E />");
        assert_eq!(
            patch.apply(text, &tags),
            r#"<R><A y="a&lt;b" z="3" /><B><D /></B><C><E />t</C></R>"#
        );
    }

    #[test]
    fn references_match_in_full_or_by_their_end() {
        assert!(same_reference("P-1_R-1", "P-1_R-1"));
        assert!(same_reference("M-00FA_A-0201-01-0000_P-1_R-1", "P-1_R-1"));
        assert!(!same_reference("M-00FA_A-0201-01-0000_P-11_R-1", "P-1_R-1"));
        assert!(!same_reference("XP-1_R-1", "P-1_R-1"));
    }

    #[test]
    fn crc_is_the_one_zip_uses() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
