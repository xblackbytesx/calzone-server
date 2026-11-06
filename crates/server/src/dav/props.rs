//! Live properties for each kind of resource.

use calzone_dav::path::{self, Kind};
use calzone_dav::request::PropRequest;
use calzone_dav::response::{Prop, Response, Value};
use calzone_dav::xml::{CALDAV, CARDDAV, CS, DAV, ICAL, QName, escape};
use calzone_dav::{MAX_RESOURCE_SIZE, sync_token};

use crate::domain::objects::ObjectMeta;
use crate::domain::{Access, Collection, CollectionKind};

pub enum Resource<'a> {
    Root,
    Principal { name: &'a str, display_name: &'a str },
    Home(Kind),
    Collection(&'a Collection),
    Object { meta: &'a ObjectMeta, body: Option<&'a str> },
}

pub fn kind_of(c: &Collection) -> Kind {
    match c.kind {
        CollectionKind::Calendar => Kind::Calendar,
        CollectionKind::AddressBook => Kind::AddressBook,
    }
}

pub fn collection_href(c: &Collection) -> String {
    path::collection_href(kind_of(c), &c.owner_name, &c.slug)
}

pub fn object_href(c: &Collection, name: &str) -> String {
    path::object_href(kind_of(c), &c.owner_name, &c.slug, name)
}

pub fn content_type(meta: &ObjectMeta) -> String {
    match meta.kind.as_str() {
        "VCARD" => "text/vcard; charset=utf-8".into(),
        k => format!("text/calendar; charset=utf-8; component={}", k.to_ascii_lowercase()),
    }
}

/// The privileges for an access level (RFC 3744). Write is content only;
/// renaming and recolouring (`write-properties`) needs admin.
fn privileges(access: Access) -> String {
    let mut p = vec!["read", "read-current-user-privilege-set"];
    if access.can_write() {
        p.extend(["write-content", "bind", "unbind"]);
    }
    if access.can_admin() {
        p.extend(["write", "write-properties"]);
    }
    if access == Access::Owner {
        p.push("all");
    }
    p.iter().map(|x| format!("<d:privilege><d:{x}/></d:privilege>")).collect()
}

fn report(ns_prefix: &str, name: &str) -> String {
    format!("<d:supported-report><d:report><{ns_prefix}:{name}/></d:report></d:supported-report>")
}

fn q(ns: &str, local: &str) -> QName {
    QName::new(ns, local)
}

/// Every property a resource has (for `allprop` and `propname`). Object data
/// is left out of `allprop`, as RFC 4791 asks.
fn names(res: &Resource<'_>) -> Vec<QName> {
    let mut n = vec![q(DAV, "resourcetype"), q(DAV, "current-user-principal")];
    match res {
        Resource::Root => n.push(q(DAV, "displayname")),
        Resource::Principal { .. } => n.extend([
            q(DAV, "displayname"),
            q(DAV, "principal-URL"),
            q(DAV, "group-membership"),
            q(CALDAV, "calendar-home-set"),
            q(CARDDAV, "addressbook-home-set"),
        ]),
        Resource::Home(_) => n.extend([q(DAV, "displayname"), q(DAV, "current-user-privilege-set")]),
        Resource::Collection(c) => {
            n.extend([
                q(DAV, "displayname"),
                q(DAV, "owner"),
                q(DAV, "current-user-privilege-set"),
                q(DAV, "supported-report-set"),
                q(DAV, "sync-token"),
                q(CS, "getctag"),
            ]);
            match c.kind {
                CollectionKind::Calendar => {
                    n.extend([
                        q(CALDAV, "calendar-description"),
                        q(CALDAV, "supported-calendar-component-set"),
                        q(CALDAV, "supported-calendar-data"),
                        q(CALDAV, "max-resource-size"),
                    ]);
                    if c.color.is_some() {
                        n.push(q(ICAL, "calendar-color"));
                    }
                }
                CollectionKind::AddressBook => n.extend([
                    q(CARDDAV, "addressbook-description"),
                    q(CARDDAV, "supported-address-data"),
                    q(CARDDAV, "max-resource-size"),
                ]),
            }
        }
        Resource::Object { body, .. } => {
            n.extend([q(DAV, "getetag"), q(DAV, "getcontenttype")]);
            if body.is_some() {
                n.push(q(DAV, "getcontentlength"));
            }
        }
    }
    n
}

/// One property's value, or None when this resource does not have it.
fn value(res: &Resource<'_>, me: &str, name: &QName) -> Option<Value> {
    let ns = name.ns.as_str();
    let local = name.local.as_str();
    if (ns, local) == (DAV, "current-user-principal") {
        return Some(Value::Hrefs(vec![path::principal_href(me)]));
    }
    match res {
        Resource::Root => match (ns, local) {
            (DAV, "resourcetype") => Some(Value::Xml("<d:collection/>".into())),
            (DAV, "displayname") => Some(Value::Text("CalZone".into())),
            _ => None,
        },
        Resource::Principal { name, display_name } => match (ns, local) {
            (DAV, "resourcetype") => Some(Value::Xml("<d:principal/>".into())),
            (DAV, "displayname") => {
                Some(Value::Text(if display_name.is_empty() { name.to_string() } else { display_name.to_string() }))
            }
            (DAV, "principal-URL") => Some(Value::Hrefs(vec![path::principal_href(name)])),
            // Shared collections already show up in the user's own homes.
            (DAV, "group-membership") => Some(Value::Hrefs(Vec::new())),
            (CALDAV, "calendar-home-set") => Some(Value::Hrefs(vec![path::home_href(Kind::Calendar, name)])),
            (CARDDAV, "addressbook-home-set") => Some(Value::Hrefs(vec![path::home_href(Kind::AddressBook, name)])),
            _ => None,
        },
        Resource::Home(kind) => match (ns, local) {
            (DAV, "resourcetype") => Some(Value::Xml("<d:collection/>".into())),
            (DAV, "displayname") => Some(Value::Text(
                match kind {
                    Kind::Calendar => "Calendars",
                    Kind::AddressBook => "Address books",
                }
                .into(),
            )),
            (DAV, "current-user-privilege-set") => Some(Value::Xml(
                "<d:privilege><d:read/></d:privilege><d:privilege><d:bind/></d:privilege>\
                 <d:privilege><d:unbind/></d:privilege>"
                    .into(),
            )),
            _ => None,
        },
        Resource::Collection(c) => collection_value(c, ns, local),
        Resource::Object { meta, body, .. } => match (ns, local) {
            (DAV, "resourcetype") => Some(Value::Empty),
            (DAV, "getetag") => Some(Value::Text(meta.etag())),
            (DAV, "getcontenttype") => Some(Value::Text(content_type(meta))),
            (DAV, "getcontentlength") => body.map(|b| Value::Text(b.len().to_string())),
            (CALDAV, "calendar-data") if meta.kind != "VCARD" => body.map(|b| Value::Text(b.to_string())),
            (CARDDAV, "address-data") if meta.kind == "VCARD" => body.map(|b| Value::Text(b.to_string())),
            _ => None,
        },
    }
}

fn collection_value(c: &Collection, ns: &str, local: &str) -> Option<Value> {
    let calendar = c.kind == CollectionKind::Calendar;
    let token = || sync_token(&c.id.to_string(), c.sequence);
    match (ns, local) {
        (DAV, "resourcetype") => Some(Value::Xml(
            if calendar { "<d:collection/><c:calendar/>" } else { "<d:collection/><card:addressbook/>" }.into(),
        )),
        (DAV, "displayname") => {
            Some(Value::Text(if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() }))
        }
        (DAV, "owner") => Some(Value::Hrefs(vec![path::principal_href(&c.owner_name)])),
        // A subscribed calendar is read-only for every client (decision 24).
        (DAV, "current-user-privilege-set") => {
            Some(Value::Xml(privileges(if c.subscribed { Access::Read } else { c.access })))
        }
        (DAV, "sync-token") => Some(Value::Text(token())),
        (CS, "getctag") => Some(Value::Text(token())),
        (DAV, "supported-report-set") => Some(Value::Xml(if calendar {
            report("d", "sync-collection") + &report("c", "calendar-multiget") + &report("c", "calendar-query")
        } else {
            report("d", "sync-collection")
                + &report("card", "addressbook-multiget")
                + &report("card", "addressbook-query")
        })),
        (CALDAV, "calendar-description") if calendar => Some(Value::Text(c.description.clone())),
        (CALDAV, "supported-calendar-component-set") if calendar => {
            let comps: Vec<&str> = if c.components.is_empty() {
                vec!["VEVENT", "VTODO", "VJOURNAL"]
            } else {
                c.components.iter().map(String::as_str).collect()
            };
            Some(Value::Xml(comps.iter().map(|n| format!("<c:comp name=\"{}\"/>", escape(n))).collect()))
        }
        (CALDAV, "supported-calendar-data") if calendar => {
            Some(Value::Xml("<c:calendar-data content-type=\"text/calendar\" version=\"2.0\"/>".into()))
        }
        (CALDAV, "max-resource-size") if calendar => Some(Value::Text(MAX_RESOURCE_SIZE.to_string())),
        (ICAL, "calendar-color") if calendar => c.color.clone().map(Value::Text),
        (CARDDAV, "addressbook-description") if !calendar => Some(Value::Text(c.description.clone())),
        // vCard 3.0 only: cards are stored as sent, and the household's are 3.0.
        (CARDDAV, "supported-address-data") if !calendar => {
            Some(Value::Xml("<card:address-data-type content-type=\"text/vcard\" version=\"3.0\"/>".into()))
        }
        (CARDDAV, "max-resource-size") if !calendar => Some(Value::Text(MAX_RESOURCE_SIZE.to_string())),
        _ => None,
    }
}

/// The response element for one resource.
pub fn response(href: String, res: &Resource<'_>, me: &str, req: &PropRequest) -> Response {
    match req {
        PropRequest::Names => {
            let ok = names(res).into_iter().map(|n| Prop::new(n, Value::Empty)).collect();
            Response::Props { href, ok, missing: Vec::new(), other: Vec::new() }
        }
        PropRequest::All { include } => {
            let mut wanted = names(res);
            wanted.extend(include.iter().filter(|i| !wanted.contains(i)).cloned().collect::<Vec<_>>());
            let mut ok = Vec::new();
            for n in wanted {
                if let Some(v) = value(res, me, &n) {
                    ok.push(Prop::new(n, v));
                }
            }
            Response::Props { href, ok, missing: Vec::new(), other: Vec::new() }
        }
        PropRequest::Props(list) => {
            let mut ok = Vec::new();
            let mut missing = Vec::new();
            for n in list {
                match value(res, me, n) {
                    Some(v) => ok.push(Prop::new(n.clone(), v)),
                    None => missing.push(n.clone()),
                }
            }
            Response::Props { href, ok, missing, other: Vec::new() }
        }
    }
}

/// Whether answering needs object bodies.
pub fn wants_data(req: &PropRequest) -> bool {
    req.wants(CALDAV, "calendar-data") || req.wants(CARDDAV, "address-data") || req.wants(DAV, "getcontentlength")
}
