//! Multistatus and error bodies.

use std::fmt::Write;

use crate::xml::{NS_DECLS, QName, close_tag, escape, open_tag};

/// A property's value as we write it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `<d:foo/>`
    Empty,
    /// Escaped text content.
    Text(String),
    /// One or more `<d:href>`s.
    Hrefs(Vec<String>),
    /// An XML fragment we built with the fixed prefixes (`<d:collection/>`).
    Xml(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prop {
    pub name: QName,
    pub value: Value,
}

impl Prop {
    pub fn new(name: QName, value: Value) -> Self {
        Self { name, value }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Props { href: String, ok: Vec<Prop>, missing: Vec<QName>, other: Vec<(u16, Vec<QName>)> },
    Status { href: String, status: u16 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Multistatus {
    pub responses: Vec<Response>,
    pub sync_token: Option<String>,
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        412 => "Precondition Failed",
        424 => "Failed Dependency",
        507 => "Insufficient Storage",
        _ => "Error",
    }
}

fn status_line(out: &mut String, status: u16) {
    let _ = write!(out, "<d:status>HTTP/1.1 {status} {}</d:status>", reason(status));
}

fn write_prop(out: &mut String, p: &Prop) {
    let (open, close) = (open_tag(&p.name), close_tag(&p.name));
    match &p.value {
        Value::Empty => {
            let _ = write!(out, "<{open}/>");
        }
        Value::Text(t) => {
            let _ = write!(out, "<{open}>{}</{close}>", escape(t));
        }
        Value::Hrefs(hs) => {
            let _ = write!(out, "<{open}>");
            for h in hs {
                let _ = write!(out, "<d:href>{}</d:href>", escape(h));
            }
            let _ = write!(out, "</{close}>");
        }
        Value::Xml(x) => {
            let _ = write!(out, "<{open}>{x}</{close}>");
        }
    }
}

fn write_names(out: &mut String, status: u16, names: &[QName]) {
    out.push_str("<d:propstat><d:prop>");
    for n in names {
        let _ = write!(out, "<{}/>", open_tag(n));
    }
    out.push_str("</d:prop>");
    status_line(out, status);
    out.push_str("</d:propstat>");
}

impl Multistatus {
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(256 + self.responses.len() * 256);
        let _ = write!(out, "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus {NS_DECLS}>");
        for r in &self.responses {
            match r {
                Response::Status { href, status } => {
                    let _ = write!(out, "<d:response><d:href>{}</d:href>", escape(href));
                    status_line(&mut out, *status);
                    out.push_str("</d:response>");
                }
                Response::Props { href, ok, missing, other } => {
                    let _ = write!(out, "<d:response><d:href>{}</d:href>", escape(href));
                    if !ok.is_empty() {
                        out.push_str("<d:propstat><d:prop>");
                        for p in ok {
                            write_prop(&mut out, p);
                        }
                        out.push_str("</d:prop>");
                        status_line(&mut out, 200);
                        out.push_str("</d:propstat>");
                    }
                    if !missing.is_empty() {
                        write_names(&mut out, 404, missing);
                    }
                    for (status, names) in other {
                        write_names(&mut out, *status, names);
                    }
                    if ok.is_empty() && missing.is_empty() && other.is_empty() {
                        status_line(&mut out, 200);
                    }
                    out.push_str("</d:response>");
                }
            }
        }
        if let Some(t) = &self.sync_token {
            let _ = write!(out, "<d:sync-token>{}</d:sync-token>", escape(t));
        }
        out.push_str("</d:multistatus>\n");
        out
    }
}

/// A `DAV:error` body holding one precondition element, e.g.
/// `<d:valid-sync-token/>`.
pub fn error_body(condition: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error {NS_DECLS}>{condition}</d:error>\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::{self, CALDAV, DAV};

    #[test]
    fn renders_parseable_multistatus() {
        let ms = Multistatus {
            responses: vec![
                Response::Props {
                    href: "/dav/calendars/a/b/x&y.ics".into(),
                    ok: vec![
                        Prop::new(QName::new(DAV, "getetag"), Value::Text("\"3\"".into())),
                        Prop::new(QName::new(CALDAV, "calendar-data"), Value::Text("A:1\r\nB:<2>\r\n".into())),
                        Prop::new(QName::new(DAV, "resourcetype"), Value::Xml("<d:collection/>".into())),
                    ],
                    missing: vec![QName::new("urn:x", "odd"), QName::new("", "bare")],
                    other: vec![],
                },
                Response::Status { href: "/dav/calendars/a/b/gone.ics".into(), status: 404 },
            ],
            sync_token: Some("urn:calzone:sync:c:4".into()),
        };
        let text = ms.render();
        let root = xml::parse(text.as_bytes()).unwrap();
        assert!(root.is(DAV, "multistatus"));
        let first = &root.children[0];
        assert_eq!(first.child(DAV, "href").unwrap().text, "/dav/calendars/a/b/x&y.ics");
        let ok = first.children_named(DAV, "propstat").next().unwrap();
        let data = ok.child(DAV, "prop").unwrap().child(CALDAV, "calendar-data").unwrap();
        assert_eq!(data.text, "A:1\r\nB:<2>\r\n", "CRLF survives");
        let missing = first.children_named(DAV, "propstat").nth(1).unwrap();
        assert!(missing.child(DAV, "prop").unwrap().child("urn:x", "odd").is_some());
        assert!(missing.child(DAV, "prop").unwrap().child("", "bare").is_some());
        assert_eq!(missing.child(DAV, "status").unwrap().text, "HTTP/1.1 404 Not Found");
        assert_eq!(root.child(DAV, "sync-token").unwrap().text, "urn:calzone:sync:c:4");
    }
}
