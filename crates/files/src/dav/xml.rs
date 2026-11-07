//! The XML of WebDAV (RFC 4918): request bodies read into a small tree, and
//! helpers to write answers. Only as much as the server needs.

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::NsReader;
use rustcloud_core::{Error, Result};

/// The namespace of WebDAV's own elements.
pub const DAV: &str = "DAV:";
/// Bodies nest no deeper than this…
const MAX_DEPTH: usize = 32;
/// …and have no more elements than this.
const MAX_ELEMENTS: usize = 10_000;

/// An element of a request body: its namespace, local name, children and text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub ns: String,
    pub name: String,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    fn new(ns: String, name: String) -> Self {
        Self { ns, name, children: Vec::new(), text: String::new() }
    }

    pub fn is(&self, ns: &str, name: &str) -> bool {
        self.ns == ns && self.name == name
    }

    /// The first child named `name` in the DAV: namespace.
    pub fn dav_child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.is(DAV, name))
    }
}

/// Reads a request body; `None` for an empty one.
pub fn parse(body: &[u8]) -> Result<Option<Element>> {
    let invalid = || Error::bad_request("INVALID_XML", "The request body is not valid XML");
    let text = std::str::from_utf8(body).map_err(|_| invalid())?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    // Text is kept as it is (spaces next to `&amp;` count); whitespace
    // between elements ends up in the parent's text, which users trim.
    let mut reader = NsReader::from_str(text);
    let mut open: Vec<Element> = Vec::new();
    let mut root = None;
    let mut count = 0;
    loop {
        let (resolved, event) = reader.read_resolved_event().map_err(|_| invalid())?;
        let ns = match resolved {
            ResolveResult::Bound(ns) => String::from_utf8_lossy(ns.as_ref()).into_owned(),
            _ => String::new(),
        };
        match event {
            Event::Start(_) | Event::Empty(_) if count >= MAX_ELEMENTS || open.len() >= MAX_DEPTH => {
                return Err(Error::bad_request("XML_TOO_LARGE", "The request body has too many elements"));
            }
            Event::Start(start) => {
                count += 1;
                open.push(Element::new(ns, String::from_utf8_lossy(start.local_name().as_ref()).into_owned()));
            }
            Event::Empty(start) => {
                count += 1;
                let element = Element::new(ns, String::from_utf8_lossy(start.local_name().as_ref()).into_owned());
                attach(&mut open, &mut root, element).ok_or_else(invalid)?;
            }
            Event::End(_) => {
                let element = open.pop().ok_or_else(invalid)?;
                attach(&mut open, &mut root, element).ok_or_else(invalid)?;
            }
            Event::Text(text) => {
                if let Some(element) = open.last_mut() {
                    element.text.push_str(&text.decode().map_err(|_| invalid())?);
                }
            }
            Event::CData(data) => {
                if let Some(element) = open.last_mut() {
                    element.text.push_str(&data.decode().map_err(|_| invalid())?);
                }
            }
            Event::GeneralRef(reference) => {
                let name = reference.decode().map_err(|_| invalid())?;
                let char = entity(&name).ok_or_else(invalid)?;
                if let Some(element) = open.last_mut() {
                    element.text.push(char);
                }
            }
            // Entities a DTD defines could grow without end.
            Event::DocType(_) => return Err(invalid()),
            Event::Eof => break,
            _ => {}
        }
    }
    if !open.is_empty() {
        return Err(invalid());
    }
    root.map(Some).ok_or_else(invalid)
}

/// Puts a finished element into its parent, or makes it the root; `None`
/// for a second root.
fn attach(open: &mut [Element], root: &mut Option<Element>, element: Element) -> Option<()> {
    match open.last_mut() {
        Some(parent) => parent.children.push(element),
        None if root.is_none() => *root = Some(element),
        None => return None,
    }
    Some(())
}

/// The character of `&name;`: the five XML defines, or `#…` numbers.
fn entity(name: &str) -> Option<char> {
    match name {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "apos" => Some('\''),
        "quot" => Some('"'),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix('x') {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// `text` safe inside an element or an attribute.
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for char in text.chars() {
        match char {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(char),
        }
    }
    escaped
}

/// An empty element `name` of namespace `ns`, as an answer names a property:
/// `<D:getetag/>`, or `<x:color xmlns:x="…"/>` for others.
pub fn empty_element(ns: &str, name: &str) -> String {
    if ns == DAV {
        format!("<D:{name}/>")
    } else {
        format!("<x:{name} xmlns:x=\"{}\"/>", escape(ns))
    }
}

/// A status line as answers carry it: `HTTP/1.1 200 OK`.
pub fn status_line(status: axum::http::StatusCode) -> String {
    format!("HTTP/1.1 {} {}", status.as_u16(), status.canonical_reason().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_become_trees_with_namespaces() {
        let body = br#"<?xml version="1.0" encoding="utf-8"?>
            <d:propfind xmlns:d="DAV:" xmlns:z="urn:x">
              <d:prop><d:getetag/><z:color>a &amp; b&#33;</z:color></d:prop>
            </d:propfind>"#;
        let root = parse(body).unwrap().unwrap();
        assert!(root.is(DAV, "propfind"));
        let prop = root.dav_child("prop").unwrap();
        assert!(prop.children[0].is(DAV, "getetag"));
        assert!(prop.children[1].is("urn:x", "color"));
        assert_eq!(prop.children[1].text, "a & b!");
    }

    #[test]
    fn empty_bodies_are_none_and_bad_ones_errors() {
        assert_eq!(parse(b"  ").unwrap(), None);
        assert!(parse(b"<a><b></a>").is_err());
        assert!(parse(b"<a/><b/>").is_err());
        assert!(parse(b"<!DOCTYPE a [<!ENTITY x \"y\">]><a>&x;</a>").is_err());
        let deep = "<a>".repeat(100) + &"</a>".repeat(100);
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn answers_are_escaped() {
        assert_eq!(escape("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&apos;");
        assert_eq!(empty_element(DAV, "getetag"), "<D:getetag/>");
        assert_eq!(empty_element("urn:\"x", "c"), "<x:c xmlns:x=\"urn:&quot;x\"/>");
    }
}
