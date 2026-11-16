//! A small XML reader: elements, attributes, text, comments, CDATA, declarations and the
//! predefined and numeric entities. That is all ETS writes into product and project files.
//!
//! The reader builds a tree that borrows from the input. Namespace prefixes are dropped, so
//! elements and attributes are matched by local name. Nesting is limited to [`MAX_DEPTH`], which
//! keeps every later walk over the tree within a bounded stack.

use std::borrow::Cow;

use crate::error::Error;

/// Deepest element nesting accepted. ETS product files stay far below this.
pub(crate) const MAX_DEPTH: usize = 200;

/// One element with its attributes and child elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Element<'a> {
    /// Local name, without a namespace prefix.
    pub name: &'a str,
    attributes: Vec<(&'a str, Cow<'a, str>)>,
    /// Child elements in document order.
    pub children: Vec<Element<'a>>,
    /// Text before the first child element, raw (entities not resolved).
    text: &'a str,
}

impl Element<'static> {
    /// An element without name, attributes or children; stands in for an absent one.
    pub(crate) const fn empty() -> Self {
        Self {
            name: "",
            attributes: Vec::new(),
            children: Vec::new(),
            text: "",
        }
    }
}

impl<'a> Element<'a> {
    /// The value of attribute `name`, entities resolved.
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_ref())
    }

    /// The first child element called `name`.
    pub(crate) fn child(&self, name: &str) -> Option<&Element<'a>> {
        self.children.iter().find(|child| child.name == name)
    }

    /// All child elements called `name`.
    pub(crate) fn children_named<'s>(
        &'s self,
        name: &'s str,
    ) -> impl Iterator<Item = &'s Element<'a>> {
        self.children.iter().filter(move |child| child.name == name)
    }

    /// Raw text before the first child element. Good enough for base64 payloads, which contain
    /// no entities.
    pub(crate) fn text(&self) -> &'a str {
        self.text
    }

    /// The first element called `name` in document order, this element included.
    pub(crate) fn find(&self, name: &str) -> Option<&Element<'a>> {
        let mut pending = vec![self];
        while let Some(element) = pending.pop() {
            if element.name == name {
                return Some(element);
            }
            pending.extend(element.children.iter().rev());
        }
        None
    }
}

fn error(message: impl Into<String>) -> Error {
    Error::Xml(message.into())
}

/// Parse a document and return its root element.
pub(crate) fn parse(text: &str) -> Result<Element<'_>, Error> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    // Elements whose end tag has not been seen yet, outermost first, with their qualified names.
    let mut open: Vec<(&str, Element<'_>)> = Vec::new();
    let mut root: Option<Element<'_>> = None;
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let (before, tail) = rest.split_at_checked(start).unwrap_or((rest, ""));
        note_text(&mut open, before);
        rest = tail;
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = skip_past(after, "-->")?;
        } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let end = after
                .find("]]>")
                .ok_or_else(|| error("unterminated CDATA section"))?;
            note_text(&mut open, after.get(..end).unwrap_or_default());
            rest = after.get(end + 3..).unwrap_or_default();
        } else if let Some(after) = rest.strip_prefix("<?") {
            rest = skip_past(after, "?>")?;
        } else if let Some(after) = rest.strip_prefix("<!") {
            rest = skip_past(after, ">")?;
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after
                .find('>')
                .ok_or_else(|| error("unterminated end tag"))?;
            let name = after.get(..end).unwrap_or_default().trim();
            rest = after.get(end + 1..).unwrap_or_default();
            let (qualified, element) = open
                .pop()
                .ok_or_else(|| error(format!("unexpected end tag </{name}>")))?;
            if qualified != name {
                return Err(error(format!("<{qualified}> closed by </{name}>")));
            }
            close(&mut open, &mut root, element)?;
        } else {
            let (tag, after) = start_tag(rest.get(1..).unwrap_or_default())?;
            rest = after;
            if tag.self_closing {
                close(&mut open, &mut root, tag.element)?;
            } else {
                if open.len() >= MAX_DEPTH {
                    return Err(error("elements nested too deeply"));
                }
                open.push((tag.qualified, tag.element));
            }
        }
    }
    if let Some((qualified, _)) = open.last() {
        return Err(error(format!("<{qualified}> is never closed")));
    }
    root.ok_or_else(|| error("no root element"))
}

/// Record text for the innermost open element, if it has no children yet.
fn note_text<'a>(open: &mut [(&'a str, Element<'a>)], text: &'a str) {
    if let Some((_, element)) = open.last_mut()
        && element.children.is_empty()
        && element.text.is_empty()
    {
        element.text = text;
    }
}

fn close<'a>(
    open: &mut [(&'a str, Element<'a>)],
    root: &mut Option<Element<'a>>,
    element: Element<'a>,
) -> Result<(), Error> {
    match open.last_mut() {
        Some((_, parent)) => parent.children.push(element),
        None if root.is_none() => *root = Some(element),
        None => return Err(error("more than one root element")),
    }
    Ok(())
}

struct StartTag<'a> {
    qualified: &'a str,
    element: Element<'a>,
    self_closing: bool,
}

/// Parse a start tag; `text` begins right after its `<`. Returns the tag and the input after
/// its `>`.
fn start_tag(text: &str) -> Result<(StartTag<'_>, &str), Error> {
    let name_end = text
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .ok_or_else(|| error("unterminated tag"))?;
    let qualified = text.get(..name_end).unwrap_or_default();
    if qualified.is_empty() {
        return Err(error("tag without a name"));
    }
    let mut element = Element {
        name: local_name(qualified),
        attributes: Vec::new(),
        children: Vec::new(),
        text: "",
    };
    let mut rest = text.get(name_end..).unwrap_or_default();
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("/>") {
            let tag = StartTag {
                qualified,
                element,
                self_closing: true,
            };
            return Ok((tag, after));
        }
        if let Some(after) = rest.strip_prefix('>') {
            let tag = StartTag {
                qualified,
                element,
                self_closing: false,
            };
            return Ok((tag, after));
        }
        let eq = rest
            .find(['=', '>'])
            .filter(|at| rest.get(*at..).is_some_and(|s| s.starts_with('=')))
            .ok_or_else(|| error(format!("bad attribute in <{qualified}>")))?;
        let key = rest.get(..eq).unwrap_or_default().trim();
        let after = rest.get(eq + 1..).unwrap_or_default().trim_start();
        let quote = after
            .chars()
            .next()
            .filter(|q| *q == '"' || *q == '\'')
            .ok_or_else(|| error(format!("unquoted attribute {key}")))?;
        let body = after.get(1..).unwrap_or_default();
        let end = body
            .find(quote)
            .ok_or_else(|| error(format!("unterminated attribute {key}")))?;
        let value = unescape(body.get(..end).unwrap_or_default())?;
        element.attributes.push((local_name(key), value));
        rest = body.get(end + 1..).unwrap_or_default();
    }
}

fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn skip_past<'a>(text: &'a str, marker: &str) -> Result<&'a str, Error> {
    let at = text
        .find(marker)
        .ok_or_else(|| error(format!("missing {marker:?}")))?;
    Ok(text.get(at + marker.len()..).unwrap_or_default())
}

/// Resolve the predefined and numeric entities.
pub(crate) fn unescape(text: &str) -> Result<Cow<'_, str>, Error> {
    if !text.contains('&') {
        return Ok(Cow::Borrowed(text));
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or_default());
        let after = rest.get(amp + 1..).unwrap_or_default();
        let semi = after
            .find(';')
            .ok_or_else(|| error("unterminated entity"))?;
        let entity = after.get(..semi).unwrap_or_default();
        let resolved = match entity {
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
        out.push(resolved.ok_or_else(|| error(format!("unknown entity &{entity};")))?);
        rest = after.get(semi + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    Ok(Cow::Owned(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_and_attributes() {
        let doc = r#"<?xml version="1.0"?>
            <!-- a comment with <tags> -->
            <k:Root xmlns:k="urn:x" Name="a &amp; b">
              <Item Test="&gt;5" Other='x "y"'/>
              <Item>payload<Inner/>ignored</Item>
              <Data><![CDATA[AQID]]></Data>
            </k:Root>"#;
        let root = parse(doc).unwrap();
        assert_eq!(root.name, "Root");
        assert_eq!(root.attr("Name"), Some("a & b"));
        assert_eq!(root.children.len(), 3);
        let first = &root.children[0];
        assert_eq!(first.attr("Test"), Some(">5"));
        assert_eq!(first.attr("Other"), Some("x \"y\""));
        assert_eq!(first.attr("Missing"), None);
        assert_eq!(root.children[1].text(), "payload");
        assert_eq!(root.children[1].children.len(), 1);
        assert_eq!(root.child("Data").unwrap().text(), "AQID");
        assert_eq!(root.find("Inner").unwrap().name, "Inner");
        assert_eq!(root.children_named("Item").count(), 2);
    }

    #[test]
    fn greater_than_inside_an_attribute() {
        let root = parse(r#"<a t="x>y"><b/></a>"#).unwrap();
        assert_eq!(root.attr("t"), Some("x>y"));
        assert_eq!(root.children.len(), 1);
    }

    #[test]
    fn malformed_input_is_an_error() {
        for doc in [
            "",
            "plain text",
            "<a>",
            "<a></b>",
            "</a>",
            "<a b=c/>",
            "<a b=\"c/>",
            "<a/><b/>",
            "<a>&bogus;</a><",
            "<a b=\"&bogus;\"/>",
            "<!-- never closed",
            "<a><![CDATA[x</a>",
        ] {
            assert!(parse(doc).is_err(), "{doc:?} should fail");
        }
    }

    #[test]
    fn nesting_is_limited() {
        let deep = "<a>".repeat(MAX_DEPTH + 1) + &"</a>".repeat(MAX_DEPTH + 1);
        assert!(parse(&deep).is_err());
        let ok = "<a>".repeat(MAX_DEPTH) + &"</a>".repeat(MAX_DEPTH);
        assert!(parse(&ok).is_ok());
    }

    #[test]
    fn entities() {
        assert_eq!(unescape("a &lt;b&gt; &#228;&#xE4;").unwrap(), "a <b> ää");
        assert!(unescape("&#xD800;").is_err());
        assert!(unescape("&unterminated").is_err());
    }
}
