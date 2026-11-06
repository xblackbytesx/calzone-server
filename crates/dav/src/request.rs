//! Request bodies: PROPFIND, PROPPATCH, MKCALENDAR / extended MKCOL and the
//! REPORTs DAVx5 and Thunderbird send.

use crate::filter::{CalFilter, CardFilter};
use crate::xml::{self, CALDAV, CARDDAV, DAV, Element, QName, XmlError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error(transparent)]
    Xml(#[from] XmlError),
    #[error("unexpected root element {0}")]
    UnexpectedRoot(QName),
    #[error("{0}")]
    Bad(String),
    #[error(transparent)]
    Filter(#[from] crate::filter::FilterError),
}

/// Which properties a client asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropRequest {
    /// Every live property we compute cheaply (`allprop`, or no body), plus
    /// any named in `include`.
    All {
        include: Vec<QName>,
    },
    /// Names only.
    Names,
    Props(Vec<QName>),
}

impl PropRequest {
    pub fn wants(&self, ns: &str, local: &str) -> bool {
        match self {
            PropRequest::All { include } => include.iter().any(|q| q.is(ns, local)),
            PropRequest::Names => false,
            PropRequest::Props(p) => p.iter().any(|q| q.is(ns, local)),
        }
    }
}

fn prop_names(prop: &Element) -> Vec<QName> {
    prop.children.iter().map(|c| c.name.clone()).collect()
}

/// `allprop` / `propname` / `prop` inside a PROPFIND or REPORT root.
fn prop_request(root: &Element) -> PropRequest {
    if root.child(DAV, "propname").is_some() {
        return PropRequest::Names;
    }
    if root.child(DAV, "allprop").is_some() {
        let include = root.child(DAV, "include").map(prop_names).unwrap_or_default();
        return PropRequest::All { include };
    }
    match root.child(DAV, "prop") {
        Some(p) => PropRequest::Props(prop_names(p)),
        None => PropRequest::All { include: Vec::new() },
    }
}

/// An empty body means `allprop` (RFC 4918 9.1).
pub fn parse_propfind(body: &[u8]) -> Result<PropRequest, RequestError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(PropRequest::All { include: Vec::new() });
    }
    let root = xml::parse(body)?;
    if !root.is(DAV, "propfind") {
        return Err(RequestError::UnexpectedRoot(root.name));
    }
    Ok(prop_request(&root))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchOp {
    Set(Element),
    Remove(QName),
}

/// The set and remove instructions, in document order.
pub fn parse_proppatch(body: &[u8]) -> Result<Vec<PatchOp>, RequestError> {
    let root = xml::parse(body)?;
    if !root.is(DAV, "propertyupdate") {
        return Err(RequestError::UnexpectedRoot(root.name));
    }
    let mut ops = Vec::new();
    for action in &root.children {
        let set = action.is(DAV, "set");
        if !set && !action.is(DAV, "remove") {
            continue;
        }
        for prop in action.children_named(DAV, "prop") {
            for p in &prop.children {
                ops.push(if set { PatchOp::Set(p.clone()) } else { PatchOp::Remove(p.name.clone()) });
            }
        }
    }
    Ok(ops)
}

/// MKCALENDAR (`C:mkcalendar`) or extended MKCOL (`D:mkcol`, RFC 5689).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MakeCollection {
    /// The children of `resourcetype`, if the body set one.
    pub resourcetype: Option<Vec<QName>>,
    /// Every other property to set.
    pub props: Vec<Element>,
}

pub fn parse_mkcol(body: &[u8]) -> Result<MakeCollection, RequestError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(MakeCollection::default());
    }
    let root = xml::parse(body)?;
    if !(root.is(CALDAV, "mkcalendar") || root.is(DAV, "mkcol")) {
        return Err(RequestError::UnexpectedRoot(root.name));
    }
    let mut out = MakeCollection::default();
    for set in root.children_named(DAV, "set") {
        for prop in set.children_named(DAV, "prop") {
            for p in &prop.children {
                if p.is(DAV, "resourcetype") {
                    out.resourcetype = Some(prop_names(p));
                } else {
                    out.props.push(p.clone());
                }
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// RFC 6578. An empty token is the initial sync.
    SyncCollection {
        token: String,
        props: PropRequest,
        infinite: bool,
    },
    CalendarMultiget {
        props: PropRequest,
        hrefs: Vec<String>,
    },
    CalendarQuery {
        props: PropRequest,
        filter: CalFilter,
    },
    AddressbookMultiget {
        props: PropRequest,
        hrefs: Vec<String>,
    },
    AddressbookQuery {
        props: PropRequest,
        filter: CardFilter,
    },
    /// Anything else (expand-property, free-busy-query, ...).
    Unsupported(QName),
}

pub fn parse_report(body: &[u8]) -> Result<Report, RequestError> {
    let root = xml::parse(body)?;
    let props = prop_request(&root);
    let hrefs = || root.children_named(DAV, "href").map(|h| h.trimmed_text().to_string()).collect();
    Ok(if root.is(DAV, "sync-collection") {
        let token = root.child(DAV, "sync-token").map(|t| t.trimmed_text().to_string()).unwrap_or_default();
        let infinite = root.child(DAV, "sync-level").is_some_and(|l| l.trimmed_text() == "infinite");
        Report::SyncCollection { token, props, infinite }
    } else if root.is(CALDAV, "calendar-multiget") {
        Report::CalendarMultiget { props, hrefs: hrefs() }
    } else if root.is(CARDDAV, "addressbook-multiget") {
        Report::AddressbookMultiget { props, hrefs: hrefs() }
    } else if root.is(CALDAV, "calendar-query") {
        let filter = match root.child(CALDAV, "filter") {
            Some(f) => CalFilter::parse(f)?,
            None => return Err(RequestError::Bad("calendar-query needs a filter".into())),
        };
        Report::CalendarQuery { props, filter }
    } else if root.is(CARDDAV, "addressbook-query") {
        let filter = match root.child(CARDDAV, "filter") {
            Some(f) => CardFilter::parse(f)?,
            None => CardFilter::default(),
        };
        Report::AddressbookQuery { props, filter }
    } else {
        Report::Unsupported(root.name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propfind_forms() {
        assert_eq!(parse_propfind(b"").unwrap(), PropRequest::All { include: vec![] });
        let p = parse_propfind(br#"<propfind xmlns="DAV:"><propname/></propfind>"#).unwrap();
        assert_eq!(p, PropRequest::Names);
        let p = parse_propfind(
            br#"<d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/">
                 <d:prop><d:getetag/><cs:getctag/></d:prop></d:propfind>"#,
        )
        .unwrap();
        assert!(p.wants(DAV, "getetag") && p.wants(xml::CS, "getctag") && !p.wants(DAV, "displayname"));
        assert!(parse_propfind(br#"<x xmlns="DAV:"/>"#).is_err());
    }

    #[test]
    fn sync_collection_as_davx5_sends_it() {
        let r = parse_report(
            br#"<?xml version='1.0' encoding='UTF-8' ?><sync-collection xmlns="DAV:">
                <sync-token>urn:calzone:sync:x:3</sync-token><sync-level>1</sync-level>
                <prop><getetag /></prop></sync-collection>"#,
        )
        .unwrap();
        match r {
            Report::SyncCollection { token, props, infinite } => {
                assert_eq!(token, "urn:calzone:sync:x:3");
                assert!(props.wants(DAV, "getetag"));
                assert!(!infinite);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn multiget_hrefs() {
        let r = parse_report(
            br#"<C:calendar-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
                <D:prop><D:getetag/><C:calendar-data/></D:prop>
                <D:href>/dav/calendars/a/b/1.ics</D:href><D:href> /dav/calendars/a/b/2.ics </D:href>
                </C:calendar-multiget>"#,
        )
        .unwrap();
        let Report::CalendarMultiget { props, hrefs } = r else { panic!() };
        assert!(props.wants(CALDAV, "calendar-data"));
        assert_eq!(hrefs, ["/dav/calendars/a/b/1.ics", "/dav/calendars/a/b/2.ics"]);
    }

    #[test]
    fn proppatch_and_mkcalendar() {
        let ops = parse_proppatch(
            br##"<d:propertyupdate xmlns:d="DAV:" xmlns:i="http://apple.com/ns/ical/">
                <d:set><d:prop><d:displayname>Family</d:displayname><i:calendar-color>#ff0000</i:calendar-color></d:prop></d:set>
                <d:remove><d:prop><i:calendar-order/></d:prop></d:remove></d:propertyupdate>"##,
        )
        .unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[2], PatchOp::Remove(q) if q.is(xml::ICAL, "calendar-order")));
        let mk = parse_mkcol(
            br#"<C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:set><D:prop>
                <D:displayname>Work</D:displayname>
                <C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set>
                </D:prop></D:set></C:mkcalendar>"#,
        )
        .unwrap();
        assert_eq!(mk.resourcetype, None);
        assert_eq!(mk.props.len(), 2);
        let mk = parse_mkcol(
            br#"<D:mkcol xmlns:D="DAV:" xmlns:R="urn:ietf:params:xml:ns:carddav"><D:set><D:prop>
                <D:resourcetype><D:collection/><R:addressbook/></D:resourcetype>
                </D:prop></D:set></D:mkcol>"#,
        )
        .unwrap();
        assert_eq!(mk.resourcetype.unwrap()[1], QName::new(CARDDAV, "addressbook"));
    }
}
