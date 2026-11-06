//! The door's URL space, and hrefs in and out.
//!
//! ```text
//! /dav/                                     root (current-user-principal)
//! /dav/principals/<user>/                   principal
//! /dav/calendars/<user>/                    calendar home: every calendar the user can see
//! /dav/calendars/<owner>/<slug>/            a calendar
//! /dav/calendars/<owner>/<slug>/<name>      an event or task
//! /dav/addressbooks/...                     the same for address books and cards
//! ```
//!
//! Shared collections are listed in the sharee's home at their owner's path
//! (decision 28), so a collection has exactly one URL.

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};

pub const PREFIX: &str = "/dav";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Calendar,
    AddressBook,
}

impl Kind {
    fn segment(self) -> &'static str {
        match self {
            Kind::Calendar => "calendars",
            Kind::AddressBook => "addressbooks",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Root,
    Principal { user: String },
    Home { kind: Kind, user: String },
    Collection { kind: Kind, owner: String, slug: String },
    Object { kind: Kind, owner: String, slug: String, name: String },
}

/// Everything outside RFC 3986 `pchar` is encoded; `@`, `:` and the
/// sub-delimiters stay as clients usually send them.
const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

pub fn encode_segment(s: &str) -> String {
    utf8_percent_encode(s, SEGMENT).to_string()
}

/// A request path (still percent-encoded) to its target. `None` is outside
/// the door's URL space.
pub fn parse_path(path: &str) -> Option<Target> {
    let rest = path.strip_prefix(PREFIX)?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    let trimmed = rest.trim_start_matches('/');
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    if trimmed.is_empty() {
        return Some(Target::Root);
    }
    let mut segs = Vec::new();
    for raw in trimmed.split('/') {
        let s = percent_decode_str(raw).decode_utf8().ok()?;
        if s.is_empty() || s.contains('/') || s == "." || s == ".." {
            return None;
        }
        segs.push(s.into_owned());
    }
    let kind = |s: &str| match s {
        "calendars" => Some(Kind::Calendar),
        "addressbooks" => Some(Kind::AddressBook),
        _ => None,
    };
    let mut it = segs.into_iter();
    let first = it.next()?;
    let rest: Vec<String> = it.collect();
    match (first.as_str(), rest.as_slice()) {
        ("principals", [user]) => Some(Target::Principal { user: user.to_ascii_lowercase() }),
        (k, [user]) => Some(Target::Home { kind: kind(k)?, user: user.to_ascii_lowercase() }),
        (k, [owner, slug]) => {
            Some(Target::Collection { kind: kind(k)?, owner: owner.to_ascii_lowercase(), slug: slug.clone() })
        }
        (k, [owner, slug, name]) => Some(Target::Object {
            kind: kind(k)?,
            owner: owner.to_ascii_lowercase(),
            slug: slug.clone(),
            name: name.clone(),
        }),
        _ => None,
    }
}

/// An href as a client sent it (an absolute path or a full URL) to its target.
pub fn parse_href(href: &str) -> Option<Target> {
    let href = href.trim();
    let path = match href.find("://") {
        Some(i) => {
            let after = &href[i + 3..];
            &after[after.find('/')?..]
        }
        None => href,
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    parse_path(path)
}

pub fn principal_href(user: &str) -> String {
    format!("{PREFIX}/principals/{}/", encode_segment(user))
}

pub fn home_href(kind: Kind, user: &str) -> String {
    format!("{PREFIX}/{}/{}/", kind.segment(), encode_segment(user))
}

pub fn collection_href(kind: Kind, owner: &str, slug: &str) -> String {
    format!("{PREFIX}/{}/{}/{}/", kind.segment(), encode_segment(owner), encode_segment(slug))
}

pub fn object_href(kind: Kind, owner: &str, slug: &str, name: &str) -> String {
    format!("{}{}", collection_href(kind, owner, slug), encode_segment(name))
}

pub fn href(t: &Target) -> String {
    match t {
        Target::Root => format!("{PREFIX}/"),
        Target::Principal { user } => principal_href(user),
        Target::Home { kind, user } => home_href(*kind, user),
        Target::Collection { kind, owner, slug } => collection_href(*kind, owner, slug),
        Target::Object { kind, owner, slug, name } => object_href(*kind, owner, slug, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(name: &str) -> Target {
        Target::Object { kind: Kind::Calendar, owner: "anna".into(), slug: "family".into(), name: name.into() }
    }

    #[test]
    fn paths() {
        assert_eq!(parse_path("/dav"), Some(Target::Root));
        assert_eq!(parse_path("/dav/"), Some(Target::Root));
        assert_eq!(parse_path("/davx"), None);
        assert_eq!(parse_path("/dav/principals/Anna/"), Some(Target::Principal { user: "anna".into() }));
        assert_eq!(
            parse_path("/dav/addressbooks/anna"),
            Some(Target::Home { kind: Kind::AddressBook, user: "anna".into() })
        );
        assert_eq!(
            parse_path("/dav/calendars/anna/family/"),
            Some(Target::Collection { kind: Kind::Calendar, owner: "anna".into(), slug: "family".into() })
        );
        assert_eq!(parse_path("/dav/calendars/anna/family/a%20b%40c.ics"), Some(obj("a b@c.ics")));
        assert_eq!(parse_path("/dav/calendars/anna/family/a%2Fb.ics"), None, "no slash inside a name");
        assert_eq!(parse_path("/dav/calendars/anna/family/.."), None);
        assert_eq!(parse_path("/dav/calendars//family/"), None);
        assert_eq!(parse_path("/dav/files/anna/"), None);
        assert_eq!(parse_path("/dav/calendars/anna/family/x/y"), None);
    }

    #[test]
    fn hrefs_round_trip() {
        for name in ["plain.ics", "a b@c.ics", "é#?%.vcf", "x:y;z=1.ics"] {
            let h = href(&obj(name));
            assert_eq!(parse_href(&h), Some(obj(name)), "{h}");
            assert_eq!(parse_href(&format!("https://cal.example.com{h}")), Some(obj(name)));
        }
        assert_eq!(href(&obj("a b@c.ics")), "/dav/calendars/anna/family/a%20b@c.ics");
    }
}
