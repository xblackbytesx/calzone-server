//! `calendar-query` and `addressbook-query` filters: the subset clients send.
//!
//! Supported: a component filter (VEVENT, VTODO, VJOURNAL) with an optional
//! time range and property filters (defined, not defined, text match). Any
//! other filter element is `Unsupported`, which the door answers with
//! `403 supported-filter` instead of guessing.
//!
//! Time ranges never miss an object: the core's projection is a superset,
//! and a recurring event in a closed range is then expanded (with the
//! floating-time margin) to drop series that have no instance in it.

use calzone_core::lines::{Component, Document};
use calzone_core::object::{self, Bounds, FLOATING_MARGIN};
use calzone_core::recur;
use chrono::NaiveDateTime;

use crate::xml::{CALDAV, CARDDAV, Element};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    #[error("unsupported filter: {0}")]
    Unsupported(String),
    #[error("bad filter: {0}")]
    Bad(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeRange {
    pub start: Option<NaiveDateTime>,
    pub end: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchType {
    Equals,
    Contains,
    StartsWith,
    EndsWith,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMatch {
    pub value: String,
    /// `i;ascii-casemap` (the default) rather than `i;octet`.
    pub casemap: bool,
    pub kind: MatchType,
    pub negate: bool,
}

impl TextMatch {
    fn parse(el: &Element) -> Result<Self, FilterError> {
        let casemap = match el.attr("collation").unwrap_or("i;ascii-casemap") {
            "i;ascii-casemap" | "i;unicode-casemap" => true,
            "i;octet" => false,
            other => return Err(FilterError::Unsupported(format!("collation {other}"))),
        };
        let kind = match el.attr("match-type").unwrap_or("contains") {
            "equals" => MatchType::Equals,
            "contains" => MatchType::Contains,
            "starts-with" => MatchType::StartsWith,
            "ends-with" => MatchType::EndsWith,
            other => return Err(FilterError::Bad(format!("match-type {other}"))),
        };
        let negate = el.attr("negate-condition") == Some("yes");
        Ok(TextMatch { value: el.text.clone(), casemap, kind, negate })
    }

    fn matches(&self, text: &str) -> bool {
        let (hay, needle) = if self.casemap {
            (text.to_lowercase(), self.value.to_lowercase())
        } else {
            (text.to_string(), self.value.clone())
        };
        let hit = match self.kind {
            MatchType::Equals => hay == needle,
            MatchType::Contains => hay.contains(&needle),
            MatchType::StartsWith => hay.starts_with(&needle),
            MatchType::EndsWith => hay.ends_with(&needle),
        };
        hit != self.negate
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropTest {
    Defined,
    NotDefined,
    /// Several text matches (vCard), all or any of them.
    Text {
        all_of: bool,
        matches: Vec<TextMatch>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropFilter {
    /// Uppercased property name.
    pub name: String,
    pub test: PropTest,
}

impl PropFilter {
    fn parse(el: &Element, ns: &str) -> Result<Self, FilterError> {
        let name = el.attr("name").ok_or_else(|| FilterError::Bad("prop-filter without name".into()))?;
        let mut matches = Vec::new();
        let mut not_defined = false;
        for c in &el.children {
            if c.is(ns, "is-not-defined") {
                not_defined = true;
            } else if c.is(ns, "text-match") {
                matches.push(TextMatch::parse(c)?);
            } else {
                return Err(FilterError::Unsupported(format!("{} in prop-filter", c.name.local)));
            }
        }
        let test = match (not_defined, matches.is_empty()) {
            (true, true) => PropTest::NotDefined,
            (true, false) => return Err(FilterError::Bad("is-not-defined with text-match".into())),
            (false, true) => PropTest::Defined,
            (false, false) => PropTest::Text { all_of: el.attr("test") == Some("allof"), matches },
        };
        Ok(PropFilter { name: name.to_ascii_uppercase(), test })
    }

    fn matches(&self, comp: &Component) -> bool {
        let mut values = comp.properties().filter(|l| l.name().eq_ignore_ascii_case(&self.name)).peekable();
        match &self.test {
            PropTest::Defined => values.peek().is_some(),
            PropTest::NotDefined => values.peek().is_none(),
            PropTest::Text { all_of, matches } => values.any(|l| {
                let v = object::unescape(l.value());
                if *all_of { matches.iter().all(|m| m.matches(&v)) } else { matches.iter().any(|m| m.matches(&v)) }
            }),
        }
    }
}

/// A `calendar-query` filter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CalFilter {
    /// The component type asked for; None is any object.
    pub component: Option<String>,
    pub time_range: Option<TimeRange>,
    pub props: Vec<PropFilter>,
    /// `VCALENDAR` with `is-not-defined`: matches nothing.
    pub nothing: bool,
}

fn time(s: Option<&str>) -> Result<Option<NaiveDateTime>, FilterError> {
    match s {
        None => Ok(None),
        Some(s) => NaiveDateTime::parse_from_str(s.trim(), "%Y%m%dT%H%M%SZ")
            .map(Some)
            .map_err(|_| FilterError::Bad(format!("time-range value {s:?} is not a UTC date-time"))),
    }
}

impl CalFilter {
    pub fn parse(filter: &Element) -> Result<Self, FilterError> {
        let mut cals = filter.children_named(CALDAV, "comp-filter");
        let cal = cals.next().ok_or_else(|| FilterError::Bad("filter without comp-filter".into()))?;
        if cals.next().is_some() || !cal.attr("name").is_some_and(|n| n.eq_ignore_ascii_case("VCALENDAR")) {
            return Err(FilterError::Bad("the filter must hold one VCALENDAR comp-filter".into()));
        }
        let mut out = CalFilter::default();
        let mut inner = None;
        for c in &cal.children {
            if c.is(CALDAV, "is-not-defined") {
                out.nothing = true;
            } else if c.is(CALDAV, "comp-filter") && inner.is_none() {
                inner = Some(c);
            } else {
                return Err(FilterError::Unsupported(format!("{} on VCALENDAR", c.name.local)));
            }
        }
        let Some(comp) = inner else { return Ok(out) };
        let name = comp.attr("name").unwrap_or_default().to_ascii_uppercase();
        if !matches!(name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL") {
            return Err(FilterError::Unsupported(format!("comp-filter {name}")));
        }
        out.component = Some(name);
        for c in &comp.children {
            if c.is(CALDAV, "time-range") && out.time_range.is_none() {
                let range = TimeRange { start: time(c.attr("start"))?, end: time(c.attr("end"))? };
                if range.start.is_none() && range.end.is_none() {
                    return Err(FilterError::Bad("time-range without start or end".into()));
                }
                out.time_range = Some(range);
            } else if c.is(CALDAV, "prop-filter") {
                let p = PropFilter::parse(c, CALDAV)?;
                if matches!(&p.test, PropTest::Text { matches, .. } if matches.len() > 1) {
                    return Err(FilterError::Bad("one text-match per prop-filter".into()));
                }
                out.props.push(p);
            } else {
                return Err(FilterError::Unsupported(format!("{} in comp-filter", c.name.local)));
            }
        }
        Ok(out)
    }

    /// Whether a stored calendar object matches.
    pub fn matches(&self, body: &str) -> bool {
        if self.nothing {
            return false;
        }
        let Ok(doc) = Document::parse(body) else { return false };
        let Some(cal) = doc.components.first() else { return false };
        let comps: Vec<&Component> =
            cal.components().filter(|c| self.component.as_deref().is_none_or(|k| c.name == k)).collect();
        if self.component.is_some() && comps.is_empty() {
            return false;
        }
        if !self.props.is_empty() && !comps.iter().any(|c| self.props.iter().all(|p| p.matches(c))) {
            return false;
        }
        match &self.time_range {
            None => true,
            Some(range) => in_range(&doc, body, range),
        }
    }
}

fn in_range(doc: &Document, body: &str, range: &TimeRange) -> bool {
    let Ok(info) = object::inspect_calendar(doc, body) else { return true };
    let (start, end) = match info.bounds {
        Bounds::Undated => return true,
        Bounds::Span { start, end } => (start, end),
    };
    let overlaps = range.end.is_none_or(|re| start < re)
        && range.start.is_none_or(|rs| end.is_none_or(|e| e > rs || (e == start && start >= rs)));
    if !overlaps {
        return false;
    }
    // A series inside a closed range: expand it, so a weekly event is not
    // reported for a week it skips. Wider by the floating margin, so a
    // floating or all-day instance is never dropped for any viewer.
    let (Some(rs), Some(re)) = (range.start, range.end) else { return true };
    if !info.recurring || info.kind != object::Kind::Event {
        return true;
    }
    match recur::expand(body, rs - FLOATING_MARGIN, re + FLOATING_MARGIN, chrono_tz::UTC) {
        Ok(exp) => !exp.occurrences.is_empty() || !exp.warnings.is_empty(),
        Err(_) => true,
    }
}

/// An `addressbook-query` filter. No property filters matches every card.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CardFilter {
    pub all_of: bool,
    pub props: Vec<PropFilter>,
}

impl CardFilter {
    pub fn parse(filter: &Element) -> Result<Self, FilterError> {
        let mut out = CardFilter { all_of: filter.attr("test") == Some("allof"), props: Vec::new() };
        for c in &filter.children {
            if c.is(CARDDAV, "prop-filter") {
                out.props.push(PropFilter::parse(c, CARDDAV)?);
            } else {
                return Err(FilterError::Unsupported(format!("{} in addressbook filter", c.name.local)));
            }
        }
        Ok(out)
    }

    pub fn matches(&self, body: &str) -> bool {
        if self.props.is_empty() {
            return true;
        }
        let Ok(doc) = Document::parse(body) else { return false };
        let Some(card) = doc.components.first() else { return false };
        if self.all_of {
            self.props.iter().all(|p| p.matches(card))
        } else {
            self.props.iter().any(|p| p.matches(card))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    fn cal_filter(inner: &str) -> Result<CalFilter, FilterError> {
        let x = format!(
            r#"<C:filter xmlns:C="urn:ietf:params:xml:ns:caldav"><C:comp-filter name="VCALENDAR">{inner}</C:comp-filter></C:filter>"#
        );
        CalFilter::parse(&xml::parse(x.as_bytes()).unwrap())
    }

    const WEEKLY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//t//EN\r\nBEGIN:VEVENT\r\nUID:w\r\n\
        DTSTAMP:20260101T000000Z\r\nDTSTART:20260105T090000Z\r\nDTEND:20260105T100000Z\r\n\
        RRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:Standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    const TODO: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//t//EN\r\nBEGIN:VTODO\r\nUID:t\r\n\
        DTSTAMP:20260101T000000Z\r\nSUMMARY:Buy milk\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

    #[test]
    fn component_filter() {
        let f = cal_filter(r#"<C:comp-filter name="VEVENT"/>"#).unwrap();
        assert!(f.matches(WEEKLY));
        assert!(!f.matches(TODO));
        let any = cal_filter("").unwrap();
        assert!(any.matches(WEEKLY) && any.matches(TODO));
    }

    #[test]
    fn time_range_expands_series() {
        let range = |s: &str, e: &str| {
            cal_filter(&format!(
                r#"<C:comp-filter name="VEVENT"><C:time-range start="{s}" end="{e}"/></C:comp-filter>"#
            ))
            .unwrap()
        };
        assert!(range("20260112T000000Z", "20260113T000000Z").matches(WEEKLY), "second instance");
        assert!(!range("20260107T000000Z", "20260111T000000Z").matches(WEEKLY), "between instances");
        assert!(!range("20260201T000000Z", "20260301T000000Z").matches(WEEKLY), "after the last");
        assert!(!range("20251201T000000Z", "20251231T000000Z").matches(WEEKLY), "before the first");
        let open =
            cal_filter(r#"<C:comp-filter name="VEVENT"><C:time-range start="20260110T000000Z"/></C:comp-filter>"#)
                .unwrap();
        assert!(open.matches(WEEKLY));
        let todos =
            cal_filter(r#"<C:comp-filter name="VTODO"><C:time-range start="20260110T000000Z"/></C:comp-filter>"#)
                .unwrap();
        assert!(todos.matches(TODO), "an undated task is in every range");
    }

    #[test]
    fn prop_filters() {
        let uid = |m: &str| {
            cal_filter(&format!(
                r#"<C:comp-filter name="VEVENT"><C:prop-filter name="UID"><C:text-match collation="i;octet" match-type="equals">{m}</C:text-match></C:prop-filter></C:comp-filter>"#
            ))
            .unwrap()
        };
        assert!(uid("w").matches(WEEKLY));
        assert!(!uid("W").matches(WEEKLY));
        let undone = cal_filter(
            r#"<C:comp-filter name="VTODO"><C:prop-filter name="COMPLETED"><C:is-not-defined/></C:prop-filter></C:comp-filter>"#,
        )
        .unwrap();
        assert!(undone.matches(TODO));
    }

    #[test]
    fn unsupported_is_reported() {
        let alarm = cal_filter(r#"<C:comp-filter name="VEVENT"><C:comp-filter name="VALARM"/></C:comp-filter>"#);
        assert!(matches!(alarm, Err(FilterError::Unsupported(_))));
        let param = cal_filter(
            r#"<C:comp-filter name="VEVENT"><C:prop-filter name="ATTENDEE"><C:param-filter name="PARTSTAT"/></C:prop-filter></C:comp-filter>"#,
        );
        assert!(matches!(param, Err(FilterError::Unsupported(_))));
        let bad_time = cal_filter(r#"<C:comp-filter name="VEVENT"><C:time-range start="2026-01-01"/></C:comp-filter>"#);
        assert!(matches!(bad_time, Err(FilterError::Bad(_))));
    }

    #[test]
    fn card_filter() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c1\r\nFN:Ada Lovelace\r\nEMAIL:ada@example.org\r\nEND:VCARD\r\n";
        let f = |x: &str| {
            let x = format!(r#"<C:filter xmlns:C="urn:ietf:params:xml:ns:carddav" {x}</C:filter>"#);
            CardFilter::parse(&xml::parse(x.as_bytes()).unwrap()).unwrap()
        };
        assert!(f(">").matches(card));
        assert!(f(r#"><C:prop-filter name="FN"><C:text-match>lovelace</C:text-match></C:prop-filter>"#).matches(card));
        assert!(
            !f(r#"test="allof"><C:prop-filter name="FN"><C:text-match>ada</C:text-match></C:prop-filter><C:prop-filter name="NICKNAME"/>"#)
                .matches(card)
        );
        assert!(
            f(r#"><C:prop-filter name="FN"><C:text-match>zzz</C:text-match></C:prop-filter><C:prop-filter name="EMAIL"/>"#)
                .matches(card)
        );
    }
}
