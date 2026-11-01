//! ETS group address CSV export.
//!
//! ETS writes two layouts, with a header row whose labels follow the ETS language:
//!
//! - "1/1": one row per group address: `Group name;Address;Central;Unfiltered;Description;
//!   DatapointType;Security`.
//! - "3/1": main and middle groups get rows of their own (address `1/-/-`, `1/2/-`), and
//!   the group address name sits in the third column: `Main;Middle;Sub;Address;...`.
//!
//! The separator (semicolon, comma or tab) is taken from the header row. Without a
//! recognisable header, columns are found by content: the address is the field that parses
//! as one, the name is the last non-empty field before it, and the DPT is the field in
//! `DPST-`/`DPT-` form.

use knxide_core::GroupAddress;

use crate::{GroupAddressInfo, GroupAddressTable, ImportError, parse_dpt};

const ADDRESS: &[&str] = &[
    "address",
    "adresse",
    "group address",
    "gruppenadresse",
    "adres",
    "indirizzo",
    "dirección",
];
const NAME: &[&str] = &["group name", "name", "gruppenname", "sub", "naam", "nom"];
const DESCRIPTION: &[&str] = &[
    "description",
    "beschreibung",
    "beschrijving",
    "descrizione",
    "descripción",
];
const DPT: &[&str] = &[
    "datapointtype",
    "datapoint type",
    "dpt",
    "datenpunkttyp",
    "datapunttype",
];
const MAIN: &[&str] = &["main", "hauptgruppe", "hoofdgroep"];
const MIDDLE: &[&str] = &["middle", "mittelgruppe", "middengroep"];

struct Columns {
    address: usize,
    name: Option<usize>,
    description: Option<usize>,
    dpt: Option<usize>,
    main: Option<usize>,
    middle: Option<usize>,
}

pub(crate) fn parse(text: &str) -> Result<GroupAddressTable, ImportError> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().ok_or(ImportError::Empty)?;
    let separator = [';', '\t', ',']
        .into_iter()
        .max_by_key(|s| first.matches(*s).count())
        .unwrap_or(';');
    let header = split(first, separator);
    let columns = header_columns(&header);

    let mut table = GroupAddressTable::default();
    let mut main_name = String::new();
    let mut middle_name = String::new();
    let rows = if columns.is_some() {
        Vec::new()
    } else {
        vec![header]
    };
    for fields in rows.into_iter().chain(lines.map(|l| split(l, separator))) {
        let get = |i: Option<usize>| i.and_then(|i| fields.get(i)).map_or("", |s| s.as_str());
        let address_col = match &columns {
            Some(c) => Some(c.address),
            None => fields
                .iter()
                .position(|f| f.contains('/') && f.parse::<GroupAddress>().is_ok()),
        };
        let address_text = get(address_col);

        // 3/1 layout: range rows name the main and middle groups.
        if let Some((main, rest)) = address_text.split_once('/')
            && rest.ends_with('-')
            && main.parse::<u8>().is_ok()
        {
            if rest == "-/-" {
                main_name = first_non_empty(&fields).to_string();
                middle_name.clear();
            } else {
                middle_name = first_non_empty(&fields).to_string();
            }
            continue;
        }
        let Ok(address) = address_text.parse::<GroupAddress>() else {
            continue;
        };

        let name = match &columns {
            Some(c) if c.name.is_some() => get(c.name).to_string(),
            _ => address_col
                .and_then(|i| fields.get(..i))
                .and_then(|before| before.iter().rev().find(|f| !f.is_empty()))
                .cloned()
                .unwrap_or_default(),
        };
        let dpt = match &columns {
            Some(c) if c.dpt.is_some() => parse_dpt(get(c.dpt)),
            _ => fields
                .iter()
                .find(|f| f.starts_with("DPST-") || f.starts_with("DPT-"))
                .and_then(|f| parse_dpt(f)),
        };
        let description = columns
            .as_ref()
            .map_or("", |c| get(c.description))
            .to_string();
        let mut path = Vec::new();
        let in_layout_31 = columns
            .as_ref()
            .is_some_and(|c| c.main.is_some() || c.middle.is_some())
            || columns.is_none();
        if in_layout_31 {
            path.extend(
                [&main_name, &middle_name]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .cloned(),
            );
        }
        table.insert(GroupAddressInfo {
            address,
            name,
            description,
            dpt,
            path,
        });
    }
    if table.is_empty() && columns.is_none() {
        return Err(ImportError::UnknownFormat);
    }
    Ok(table)
}

fn first_non_empty(fields: &[String]) -> &str {
    fields
        .iter()
        .find(|f| !f.is_empty())
        .map_or("", String::as_str)
}

fn header_columns(header: &[String]) -> Option<Columns> {
    let find = |names: &[&str]| {
        header
            .iter()
            .position(|h| names.contains(&h.to_lowercase().as_str()))
    };
    let address = find(ADDRESS)?;
    Some(Columns {
        address,
        name: find(NAME),
        description: find(DESCRIPTION),
        dpt: find(DPT),
        main: find(MAIN),
        middle: find(MIDDLE),
    })
}

/// Split one CSV line, honouring double quotes and `""` escapes. Fields are trimmed.
fn split(line: &str, separator: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == separator && !quoted => {
                fields.push(std::mem::take(&mut field).trim().to_string())
            }
            c => field.push(c),
        }
    }
    fields.push(field.trim().to_string());
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_fields() {
        assert_eq!(split(r#""a;b";"c""d";e"#, ';'), ["a;b", "c\"d", "e"]);
        assert_eq!(split("x\ty\t", '\t'), ["x", "y", ""]);
    }
}
