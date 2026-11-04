//! WebDAV request bodies as a small tree with resolved namespaces, and
//! escaping for the responses (which are written as text).

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::NsReader;

pub const DAV: &str = "DAV:";
pub const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub const CALSERVER: &str = "http://calendarserver.org/ns/";
pub const APPLE: &str = "http://apple.com/ns/ical/";

/// A namespace and a local name, e.g. `("DAV:", "displayname")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub ns: String,
    pub local: String,
}

impl Name {
    pub fn is(&self, ns: &str, local: &str) -> bool {
        self.ns == ns && self.local == local
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub name: Name,
    /// Attributes by local name, e.g. `name="VEVENT"`.
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    pub fn is(&self, ns: &str, local: &str) -> bool {
        self.name.is(ns, local)
    }

    pub fn attribute(&self, local: &str) -> Option<&str> {
        self.attributes.iter().find(|(name, _)| name == local).map(|(_, value)| value.as_str())
    }

    pub fn child(&self, ns: &str, local: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.is(ns, local))
    }

    /// Every element with this name inside this one, at any depth.
    pub fn descendants<'a>(&'a self, ns: &'a str, local: &'a str) -> Vec<&'a Element> {
        let mut found = Vec::new();
        for child in &self.children {
            if child.is(ns, local) {
                found.push(child);
            }
            found.extend(child.descendants(ns, local));
        }
        found
    }
}

/// The root element of a document; `None` if it is not well-formed XML.
pub fn parse(data: &[u8]) -> Option<Element> {
    let text = std::str::from_utf8(data).ok()?;
    let mut reader = NsReader::from_str(text);
    let mut stack: Vec<Element> = Vec::new();
    loop {
        let (ns, event) = reader.read_resolved_event().ok()?;
        let ns = match ns {
            ResolveResult::Bound(ns) => String::from_utf8_lossy(ns.as_ref()).into_owned(),
            _ => String::new(),
        };
        match event {
            Event::Start(start) => stack.push(element(ns, &start)),
            Event::Empty(start) => {
                stack.push(element(ns, &start));
                close(&mut stack)?;
            }
            Event::End(_) => {
                if let Some(root) = close(&mut stack)? {
                    return Some(root);
                }
            }
            Event::Text(text) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&text.xml_content().ok()?);
                }
            }
            Event::CData(data) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&data.decode().ok()?);
                }
            }
            Event::GeneralRef(reference) => {
                let resolved = match reference.resolve_char_ref().ok()? {
                    Some(c) => c.to_string(),
                    None => resolve_predefined_entity(&reference.decode().ok()?)?.to_owned(),
                };
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&resolved);
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn element(ns: String, start: &BytesStart) -> Element {
    let local = String::from_utf8_lossy(start.local_name().as_ref()).into_owned();
    let attributes = start
        .attributes()
        .flatten()
        .filter_map(|attribute| {
            let name = String::from_utf8_lossy(attribute.key.local_name().as_ref()).into_owned();
            Some((name, attribute.unescape_value().ok()?.into_owned()))
        })
        .collect();
    Element { name: Name { ns, local }, attributes, children: Vec::new(), text: String::new() }
}

/// Ends the innermost open element; returns the root once it is closed.
fn close(stack: &mut Vec<Element>) -> Option<Option<Element>> {
    let element = stack.pop()?;
    match stack.last_mut() {
        Some(parent) => {
            parent.children.push(element);
            Some(None)
        }
        None => Some(Some(element)),
    }
}

/// Text for XML content and attribute values.
pub fn escape(text: &str) -> String {
    quick_xml::escape::escape(text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_are_resolved_and_text_unescaped() {
        let body = br#"<?xml version="1.0"?>
            <d:propfind xmlns:d="DAV:" xmlns="urn:ietf:params:xml:ns:caldav">
              <d:prop><d:displayname/><calendar-home-set/></d:prop>
              <d:href>/a&amp;b%20c &#x41;</d:href>
              <comp-filter name="VEVENT"/>
            </d:propfind>"#;
        let root = parse(body).expect("parsed");
        assert!(root.is(DAV, "propfind"));
        let prop = root.child(DAV, "prop").expect("prop");
        assert_eq!(prop.children.len(), 2);
        assert!(prop.children[1].is(CALDAV, "calendar-home-set"));
        assert_eq!(root.child(DAV, "href").unwrap().text, "/a&b%20c A");
        assert_eq!(root.descendants(DAV, "displayname").len(), 1);
        assert_eq!(root.child(CALDAV, "comp-filter").unwrap().attribute("name"), Some("VEVENT"));
    }

    #[test]
    fn broken_xml_is_none() {
        assert!(parse(b"<a><b></a>").is_none());
        assert!(parse(b"").is_none());
    }
}
