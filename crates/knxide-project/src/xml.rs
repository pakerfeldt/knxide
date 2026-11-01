//! ETS group address XML export:
//!
//! ```xml
//! <GroupAddress-Export xmlns="http://knx.org/xml/ga-export/01">
//!   <GroupRange Name="Lighting" RangeStart="2048" RangeEnd="4095">
//!     <GroupAddress Name="Kitchen" Address="1/0/1" DPTs="DPST-1-1" Description="" />
//!   </GroupRange>
//! </GroupAddress-Export>
//! ```
//!
//! A small scanner rather than a full XML parser: elements, attributes, comments,
//! declarations and the predefined and numeric entities are handled, which is all ETS writes.

use knxide_core::GroupAddress;

use crate::{GroupAddressInfo, GroupAddressTable, ImportError, parse_dpt};

pub(crate) fn parse(text: &str) -> Result<GroupAddressTable, ImportError> {
    let mut table = GroupAddressTable::default();
    // Open elements; `Some(name)` for group ranges.
    let mut stack: Vec<Option<String>> = Vec::new();
    let mut saw_root = false;
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        rest = rest.get(start..).unwrap_or_default();
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = skip_past(after, "-->")?;
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            rest = skip_past(rest, ">")?;
            continue;
        }
        let end = rest
            .find('>')
            .ok_or_else(|| ImportError::Xml("unterminated tag".into()))?;
        let tag = rest.get(1..end).unwrap_or_default();
        rest = rest.get(end + 1..).unwrap_or_default();

        if tag.starts_with('/') {
            stack.pop();
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let (name, attrs) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        let attrs = attributes(attrs)?;
        let attr = |key: &str| {
            attrs
                .iter()
                .find(|(k, _)| k == key)
                .map_or("", |(_, v)| v.as_str())
        };

        match local_name(name) {
            "GroupAddress-Export" => saw_root = true,
            "GroupAddress" => {
                let address: GroupAddress = attr("Address").parse().map_err(|_| {
                    ImportError::Xml(format!("invalid address {:?}", attr("Address")))
                })?;
                table.insert(GroupAddressInfo {
                    address,
                    name: attr("Name").to_string(),
                    description: attr("Description").to_string(),
                    dpt: parse_dpt(attr("DPTs")),
                    path: stack.iter().flatten().cloned().collect(),
                });
            }
            _ => {}
        }
        if !self_closing {
            let range = (local_name(name) == "GroupRange").then(|| attr("Name").to_string());
            stack.push(range);
        }
    }
    if !saw_root {
        return Err(ImportError::UnknownFormat);
    }
    Ok(table)
}

fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn skip_past<'a>(text: &'a str, marker: &str) -> Result<&'a str, ImportError> {
    let at = text
        .find(marker)
        .ok_or_else(|| ImportError::Xml(format!("missing {marker:?}")))?;
    Ok(text.get(at + marker.len()..).unwrap_or_default())
}

fn attributes(mut text: &str) -> Result<Vec<(String, String)>, ImportError> {
    let mut out = Vec::new();
    loop {
        text = text.trim_start();
        if text.is_empty() {
            return Ok(out);
        }
        let eq = text
            .find('=')
            .ok_or_else(|| ImportError::Xml(format!("bad attribute in {text:?}")))?;
        let key = text.get(..eq).unwrap_or_default().trim().to_string();
        let after = text.get(eq + 1..).unwrap_or_default().trim_start();
        let quote = after.chars().next().filter(|q| *q == '"' || *q == '\'');
        let quote = quote.ok_or_else(|| ImportError::Xml(format!("unquoted attribute {key}")))?;
        let body = after.get(1..).unwrap_or_default();
        let close = body
            .find(quote)
            .ok_or_else(|| ImportError::Xml(format!("unterminated attribute {key}")))?;
        out.push((key, unescape(body.get(..close).unwrap_or_default())?));
        text = body.get(close + 1..).unwrap_or_default();
    }
}

fn unescape(text: &str) -> Result<String, ImportError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or_default());
        let after = rest.get(amp + 1..).unwrap_or_default();
        let semi = after
            .find(';')
            .ok_or_else(|| ImportError::Xml("unterminated entity".into()))?;
        let entity = after.get(..semi).unwrap_or_default();
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| entity.strip_prefix('#').map(str::parse))
                .and_then(Result::ok)
                .and_then(char::from_u32),
        };
        out.push(ch.ok_or_else(|| ImportError::Xml(format!("unknown entity &{entity};")))?);
        rest = after.get(semi + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities() {
        assert_eq!(
            unescape("a &amp; b &lt;c&gt; &#228;&#xE4;").unwrap(),
            "a & b <c> ää"
        );
        assert!(unescape("&bogus;").is_err());
    }

    #[test]
    fn attribute_quotes() {
        let attrs = attributes(r#"Name="x 'y'" Address='1/2/3'"#).unwrap();
        assert_eq!(
            attrs,
            [
                ("Name".into(), "x 'y'".into()),
                ("Address".into(), "1/2/3".into())
            ]
        );
    }
}
