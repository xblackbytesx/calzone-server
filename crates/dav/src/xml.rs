//! A small namespace-resolved XML tree for request bodies, and escaping for
//! the responses we write by hand.
//!
//! Request bodies are tiny (a PROPFIND, a REPORT with hrefs), so a tree is
//! simpler than streaming. DTDs are ignored and never expanded, so entity
//! tricks (XXE, billion laughs) have nothing to work with; nesting is capped.

use std::fmt;

use quick_xml::NsReader;
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;

pub const DAV: &str = "DAV:";
pub const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
/// CalendarServer: `getctag`.
pub const CS: &str = "http://calendarserver.org/ns/";
/// Apple iCal: `calendar-color`, `calendar-order` (DAVx5 and Thunderbird read them).
pub const ICAL: &str = "http://apple.com/ns/ical/";

const MAX_DEPTH: usize = 64;

/// A namespaced element name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QName {
    pub ns: String,
    pub local: String,
}

impl QName {
    pub fn new(ns: &str, local: &str) -> Self {
        Self { ns: ns.to_string(), local: local.to_string() }
    }

    pub fn is(&self, ns: &str, local: &str) -> bool {
        self.ns == ns && self.local == local
    }
}

impl fmt::Display for QName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}{}", self.ns, self.local)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub name: QName,
    /// Attributes by local name (the ones DAV uses are unprefixed).
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// Direct text content, entities resolved, untrimmed.
    pub text: String,
}

impl Element {
    pub fn is(&self, ns: &str, local: &str) -> bool {
        self.name.is(ns, local)
    }

    pub fn child(&self, ns: &str, local: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.is(ns, local))
    }

    pub fn children_named<'a>(&'a self, ns: &'a str, local: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |c| c.is(ns, local))
    }

    pub fn attr(&self, local: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == local).map(|(_, v)| v.as_str())
    }

    pub fn trimmed_text(&self) -> &str {
        self.text.trim()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XmlError {
    #[error("malformed XML: {0}")]
    Malformed(String),
    #[error("XML nested too deeply")]
    TooDeep,
    #[error("empty XML body")]
    Empty,
}

fn bad(e: impl fmt::Display) -> XmlError {
    XmlError::Malformed(e.to_string())
}

/// Parse a document into its root element.
pub fn parse(input: &[u8]) -> Result<Element, XmlError> {
    let mut reader = NsReader::from_reader(input);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    loop {
        let (ns, event) = reader.read_resolved_event().map_err(bad)?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                if root.is_some() {
                    return Err(bad("content after the root element"));
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(XmlError::TooDeep);
                }
                let ns = match ns {
                    ResolveResult::Bound(n) => String::from_utf8_lossy(n.as_ref()).into_owned(),
                    ResolveResult::Unbound => String::new(),
                    ResolveResult::Unknown(p) => {
                        return Err(bad(format!("unknown prefix {}", String::from_utf8_lossy(&p))));
                    }
                };
                let local = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                let mut attrs = Vec::new();
                for a in e.attributes() {
                    let a = a.map_err(bad)?;
                    if a.key.as_ref().starts_with(b"xmlns") {
                        continue;
                    }
                    let k = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
                    let v = a.unescape_value().map_err(bad)?.into_owned();
                    attrs.push((k, v));
                }
                let el = Element { name: QName { ns, local }, attrs, children: Vec::new(), text: String::new() };
                if matches!(event, Event::Start(_)) {
                    stack.push(el);
                } else {
                    close(&mut stack, &mut root, el);
                }
            }
            Event::End(_) => {
                let el = stack.pop().ok_or_else(|| bad("unbalanced end tag"))?;
                close(&mut stack, &mut root, el);
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.decode().map_err(bad)?);
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.decode().map_err(bad)?);
                }
            }
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref().map_err(bad)? {
                    Some(c) => c.to_string(),
                    None => {
                        let name = r.decode().map_err(bad)?;
                        resolve_xml_entity(&name).ok_or_else(|| bad(format!("unknown entity &{name};")))?.to_string()
                    }
                };
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&resolved);
                }
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
    }
    if !stack.is_empty() {
        return Err(bad("unclosed element"));
    }
    root.ok_or(XmlError::Empty)
}

fn close(stack: &mut [Element], root: &mut Option<Element>, el: Element) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(el),
        None => *root = Some(el),
    }
}

/// Escape text for element content and attribute values. Carriage returns
/// become `&#13;` so iCalendar's CRLF survives XML newline normalisation.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

/// The prefix we write for a known namespace.
pub fn prefix(ns: &str) -> Option<&'static str> {
    match ns {
        DAV => Some("d"),
        CALDAV => Some("c"),
        CARDDAV => Some("card"),
        CS => Some("cs"),
        ICAL => Some("ical"),
        _ => None,
    }
}

/// The namespace declarations for the root of every response.
pub const NS_DECLS: &str = concat!(
    r#"xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:card="urn:ietf:params:xml:ns:carddav" "#,
    r#"xmlns:cs="http://calendarserver.org/ns/" xmlns:ical="http://apple.com/ns/ical/""#
);

/// An element tag for `name`: `d:foo`, or `x:foo xmlns:x="..."` for an
/// unknown namespace (open tag) and `x:foo` (close tag).
pub fn open_tag(name: &QName) -> String {
    match prefix(&name.ns) {
        Some(p) => format!("{p}:{}", name.local),
        None if name.ns.is_empty() => format!("{} xmlns=\"\"", name.local),
        None => format!("x:{} xmlns:x=\"{}\"", name.local, escape(&name.ns)),
    }
}

pub fn close_tag(name: &QName) -> String {
    match prefix(&name.ns) {
        Some(p) => format!("{p}:{}", name.local),
        None if name.ns.is_empty() => name.local.clone(),
        None => format!("x:{}", name.local),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_namespaces_and_entities() {
        let x = br#"<?xml version="1.0"?>
            <D:propfind xmlns:D="DAV:" xmlns="urn:ietf:params:xml:ns:caldav">
              <D:prop><D:displayname/><calendar-home-set/></D:prop>
              <D:href>/a%20b/&amp;c&#13;</D:href>
            </D:propfind>"#;
        let root = parse(x).unwrap();
        assert!(root.is(DAV, "propfind"));
        let prop = root.child(DAV, "prop").unwrap();
        assert_eq!(prop.children.len(), 2);
        assert!(prop.children[1].is(CALDAV, "calendar-home-set"));
        assert_eq!(root.child(DAV, "href").unwrap().text, "/a%20b/&c\r");
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(parse(b""), Err(XmlError::Empty));
        assert!(parse(b"<a><b></a>").is_err());
        assert!(parse(b"<x:a/>").is_err(), "unknown prefix");
        let deep = "<a>".repeat(100) + &"</a>".repeat(100);
        assert_eq!(parse(deep.as_bytes()), Err(XmlError::TooDeep));
        assert!(parse(b"<a>&ext;</a>").is_err(), "no custom entities");
        let doctype = br#"<!DOCTYPE a [<!ENTITY e "boom">]><a>&e;</a>"#;
        assert!(parse(doctype).is_err(), "DTD entities are never expanded");
    }

    #[test]
    fn escapes_carriage_returns() {
        assert_eq!(escape("a<b>&\"\r\n"), "a&lt;b&gt;&amp;&quot;&#13;\n");
        let back = parse(format!("<a>{}</a>", escape("X:1\r\nY:2\r\n")).as_bytes()).unwrap();
        assert_eq!(back.text, "X:1\r\nY:2\r\n");
    }
}
