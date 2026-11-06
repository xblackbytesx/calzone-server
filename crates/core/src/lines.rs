//! Lossless content lines (RFC 5545 section 3.1, RFC 6350 section 3.2).
//!
//! A document is a tree of components whose properties are kept as their
//! unfolded text plus, when they came from a parse, the exact source bytes
//! (folds, line endings, blank lines before them, a byte-order mark). Writing
//! a line that was never changed emits those bytes, so
//! `Document::parse(x)?.to_string() == x` for every input that parses
//! (decision 25). New or edited lines are folded at 75 octets with CRLF.

use std::fmt;

/// One logical content line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Unfolded, without the line ending, without a byte-order mark.
    text: String,
    /// The exact source of this line when it came from a parse and was not
    /// changed since: leading blank lines, folds, the line ending.
    original: Option<String>,
}

/// One parameter, values unquoted (RFC 6868 caret escapes are left as is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Uppercased.
    pub name: String,
    pub values: Vec<String>,
}

impl Line {
    /// A new line; it is folded with CRLF when written.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), original: None }
    }

    /// The unfolded text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether this line will be written from its source bytes.
    pub fn is_original(&self) -> bool {
        self.original.is_some()
    }

    /// The part before the first `:` outside quotes, and the value after it.
    fn split(&self) -> (&str, &str) {
        let mut quoted = false;
        for (i, c) in self.text.char_indices() {
            match c {
                '"' => quoted = !quoted,
                ':' if !quoted => return (&self.text[..i], &self.text[i + 1..]),
                _ => {}
            }
        }
        (&self.text, "")
    }

    /// The property name, uppercased, without its group (`item1.TEL` is `TEL`).
    pub fn name(&self) -> String {
        let head = self.split().0;
        let name = head.split(';').next().unwrap_or("");
        name.rsplit('.').next().unwrap_or(name).to_ascii_uppercase()
    }

    /// The group (`item1` in `item1.TEL`), if any.
    pub fn group(&self) -> Option<&str> {
        let head = self.split().0;
        let name = head.split(';').next().unwrap_or("");
        name.rsplit_once('.').map(|(g, _)| g)
    }

    /// The raw value: everything after the first unquoted `:`.
    pub fn value(&self) -> &str {
        self.split().1
    }

    pub fn params(&self) -> Vec<Param> {
        let head = self.split().0;
        split_unquoted(head, ';')
            .into_iter()
            .skip(1)
            .map(|p| {
                let (name, values) = p.split_once('=').unwrap_or((p, ""));
                Param {
                    name: name.to_ascii_uppercase(),
                    values: split_unquoted(values, ',').into_iter().map(|v| v.trim_matches('"').to_string()).collect(),
                }
            })
            .collect()
    }

    /// The first value of a parameter, by name (case-insensitive).
    pub fn param(&self, name: &str) -> Option<String> {
        self.params().into_iter().find(|p| p.name.eq_ignore_ascii_case(name)).and_then(|p| p.values.into_iter().next())
    }

    /// The text this line is written as (its source bytes when unchanged).
    pub fn source(&self) -> String {
        let mut s = String::new();
        let _ = self.write(&mut s);
        s
    }

    /// A line back from [`Line::source`], keeping its bytes.
    pub fn from_source(source: &str) -> Option<Self> {
        let mut lines = logical_lines(source);
        let (original, text) = lines.next()?;
        if lines.next().is_some() || text.is_empty() {
            return None;
        }
        Some(Self { text, original: Some(original.to_string()) })
    }

    fn write(&self, out: &mut impl fmt::Write) -> fmt::Result {
        match &self.original {
            Some(o) => out.write_str(o),
            None => {
                out.write_str(&fold(&self.text))?;
                out.write_str("\r\n")
            }
        }
    }
}

/// Split on `sep` outside double quotes.
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut quoted, mut start) = (false, 0);
    for (i, c) in s.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == sep && !quoted {
            parts.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Fold at 75 octets, never inside a UTF-8 character.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 70 * 3);
    let mut budget = 75;
    let mut used = 0;
    for c in text.chars() {
        let n = c.len_utf8();
        if used + n > budget {
            out.push_str("\r\n ");
            budget = 74;
            used = 0;
        }
        out.push(c);
        used += n;
    }
    out
}

/// A component and everything in it, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// Uppercased (`VEVENT`, `VCARD`).
    pub name: String,
    pub begin: Line,
    pub items: Vec<Item>,
    pub end: Line,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Property(Line),
    Component(Component),
}

impl Component {
    /// A new, empty component.
    pub fn new(name: &str) -> Self {
        let name = name.to_ascii_uppercase();
        Self {
            begin: Line::new(format!("BEGIN:{name}")),
            end: Line::new(format!("END:{name}")),
            items: Vec::new(),
            name,
        }
    }

    pub fn properties(&self) -> impl Iterator<Item = &Line> {
        self.items.iter().filter_map(|i| match i {
            Item::Property(l) => Some(l),
            Item::Component(_) => None,
        })
    }

    /// The first property with this name (case-insensitive, group ignored).
    pub fn property(&self, name: &str) -> Option<&Line> {
        self.properties().find(|l| l.name().eq_ignore_ascii_case(name))
    }

    pub fn components(&self) -> impl Iterator<Item = &Component> {
        self.items.iter().filter_map(|i| match i {
            Item::Component(c) => Some(c),
            Item::Property(_) => None,
        })
    }

    fn write(&self, out: &mut impl fmt::Write) -> fmt::Result {
        self.begin.write(out)?;
        for item in &self.items {
            match item {
                Item::Property(l) => l.write(out)?,
                Item::Component(c) => c.write(out)?,
            }
        }
        self.end.write(out)
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f)
    }
}

/// Top-level components (one `VCALENDAR`, or one or more `VCARD`s) plus any
/// blank lines after the last one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub components: Vec<Component>,
    pub trailing: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("line {line}: content outside any component")]
    OutsideComponent { line: usize },
    #[error("line {line}: END:{found} does not close BEGIN:{expected}")]
    Mismatched { line: usize, expected: String, found: String },
    #[error("line {line}: END:{found} without a BEGIN")]
    UnexpectedEnd { line: usize, found: String },
    #[error("BEGIN:{name} is never closed")]
    Unterminated { name: String },
    #[error("no components")]
    Empty,
}

impl Document {
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let mut top: Vec<Component> = Vec::new();
        let mut stack: Vec<Component> = Vec::new();
        let mut pending = String::new(); // blank lines (and a BOM) before the next line
        for (number, (original, text)) in logical_lines(input).enumerate() {
            let line_no = number + 1;
            if text.is_empty() {
                pending.push_str(original);
                continue;
            }
            let line = Line { text: text.clone(), original: Some(std::mem::take(&mut pending) + original) };
            let upper = line.text.to_ascii_uppercase();
            if let Some(name) = upper.strip_prefix("BEGIN:") {
                let name = name.trim().to_string();
                stack.push(Component { name, begin: line, items: Vec::new(), end: Line::new("") });
            } else if let Some(name) = upper.strip_prefix("END:") {
                let name = name.trim().to_string();
                let Some(mut open) = stack.pop() else {
                    return Err(ParseError::UnexpectedEnd { line: line_no, found: name });
                };
                if open.name != name {
                    return Err(ParseError::Mismatched { line: line_no, expected: open.name, found: name });
                }
                open.end = line;
                match stack.last_mut() {
                    Some(parent) => parent.items.push(Item::Component(open)),
                    None => top.push(open),
                }
            } else {
                match stack.last_mut() {
                    Some(parent) => parent.items.push(Item::Property(line)),
                    None => return Err(ParseError::OutsideComponent { line: line_no }),
                }
            }
        }
        if let Some(open) = stack.pop() {
            return Err(ParseError::Unterminated { name: open.name });
        }
        if top.is_empty() {
            return Err(ParseError::Empty);
        }
        Ok(Self { components: top, trailing: pending })
    }
}

impl fmt::Display for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in &self.components {
            c.write(f)?;
        }
        f.write_str(&self.trailing)
    }
}

/// Logical lines as (exact source slice, unfolded text). A leading BOM is part
/// of the first source slice but not of its text. Line endings are CRLF or
/// LF; a continuation line starts with a space or a tab.
fn logical_lines(input: &str) -> impl Iterator<Item = (&str, String)> {
    let mut rest = input;
    let mut first = true;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let start = rest;
        let mut consumed = 0;
        let mut text = String::new();
        let mut physical = true;
        loop {
            let line = &rest[consumed..];
            let (content, ending_len) = match line.find('\n') {
                Some(i) if i > 0 && line.as_bytes()[i - 1] == b'\r' => (&line[..i - 1], 2),
                Some(i) => (&line[..i], 1),
                None => (line, 0),
            };
            let piece = if physical {
                content
            } else {
                &content[1..] // drop the folding whitespace
            };
            text.push_str(piece);
            consumed += content.len() + ending_len;
            physical = false;
            let next = &rest[consumed..];
            // An empty line never continues: blank lines stay their own entries.
            if ending_len == 0 || text.is_empty() || !(next.starts_with(' ') || next.starts_with('\t')) {
                break;
            }
        }
        let original = &start[..consumed];
        rest = &rest[consumed..];
        if first {
            first = false;
            if let Some(t) = text.strip_prefix('\u{feff}') {
                text = t.to_string();
            }
        }
        Some((original, text))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(s: &str) {
        let doc = Document::parse(s).unwrap_or_else(|e| panic!("{e}: {s:?}"));
        assert_eq!(doc.to_string(), s);
    }

    #[test]
    fn untouched_input_round_trips_byte_for_byte() {
        round_trip("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n");
        round_trip("BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR"); // LF, no final newline
        round_trip("\u{feff}BEGIN:VCARD\r\nFN:A\r\n B\r\n\tC\r\nEND:VCARD\r\n\r\n\r\nBEGIN:VCARD\r\nEND:VCARD\r\n\r\n");
        round_trip("begin:vcard\r\nitem1.TEL;TYPE=\"a,b\":1\r\nend:VCARD\r\n");
    }

    #[test]
    fn unfolding_joins_continuations() {
        let doc = Document::parse("BEGIN:VCARD\r\nNOTE:ab\r\n cd\r\n\tef\r\nEND:VCARD\r\n").unwrap();
        assert_eq!(doc.components[0].property("note").unwrap().value(), "abcdef");
    }

    #[test]
    fn names_groups_params_and_values() {
        let l = Line::new("item1.EMAIL;TYPE=home,pref;X-Q=\"a,b\";PREF=1:a@example.org");
        assert_eq!(l.name(), "EMAIL");
        assert_eq!(l.group(), Some("item1"));
        assert_eq!(l.value(), "a@example.org");
        assert_eq!(l.param("type").as_deref(), Some("home"));
        assert_eq!(l.params()[0].values, vec!["home", "pref"]);
        // A quoted value is one value, commas included (RFC 5545 3.2).
        assert_eq!(l.params()[1].values, vec!["a,b"]);
        let l = Line::new("X-A;X-P=\"a:b\":c:d");
        assert_eq!(l.value(), "c:d");
        assert_eq!(l.param("X-P").as_deref(), Some("a:b"));
    }

    #[test]
    fn structure_errors_are_refused() {
        assert!(matches!(Document::parse("VERSION:2.0\r\n"), Err(ParseError::OutsideComponent { line: 1 })));
        assert!(matches!(Document::parse("BEGIN:A\r\nEND:B\r\n"), Err(ParseError::Mismatched { .. })));
        assert!(matches!(Document::parse("BEGIN:A\r\n"), Err(ParseError::Unterminated { .. })));
        assert!(matches!(Document::parse("END:A\r\n"), Err(ParseError::UnexpectedEnd { .. })));
        assert!(matches!(Document::parse("\r\n\r\n"), Err(ParseError::Empty)));
    }

    #[test]
    fn folding_is_75_octets_and_utf8_safe() {
        let text = format!("SUMMARY:{}", "é".repeat(100));
        let folded = fold(&text);
        for (i, physical) in folded.split("\r\n").enumerate() {
            assert!(physical.len() <= 75, "line {i} has {} octets", physical.len());
        }
        let doc = Document::parse(&format!("BEGIN:VEVENT\r\n{folded}\r\nEND:VEVENT\r\n")).unwrap();
        assert_eq!(doc.components[0].property("SUMMARY").unwrap().text(), text);
    }

    #[test]
    fn new_lines_are_written_folded_with_crlf() {
        let mut c = Component::new("vevent");
        c.items.push(Item::Property(Line::new("UID:1")));
        assert_eq!(c.to_string(), "BEGIN:VEVENT\r\nUID:1\r\nEND:VEVENT\r\n");
    }

    /// Generated documents with random folding, line endings, blank lines,
    /// multibyte text and nesting must all round-trip exactly.
    #[test]
    fn generated_documents_round_trip() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let pieces = ["a", "Z", ":", ";", "\"", ",", "=", "é", "日", " ", "\t", "\\n"];
        for _ in 0..2000 {
            let mut s = String::new();
            let eol = |r: u64| if r == 0 { "\n" } else { "\r\n" };
            let depth = 1 + next(3);
            for d in 0..depth {
                s += &format!("BEGIN:C{d}{}", eol(next(2)));
                for _ in 0..next(4) {
                    if next(4) == 0 {
                        s += eol(next(2)); // a blank line
                    }
                    s += "P";
                    for _ in 0..next(30) {
                        s += pieces[next(pieces.len() as u64) as usize];
                        if next(10) == 0 {
                            s += eol(next(2));
                            s += if next(2) == 0 { " " } else { "\t" };
                        }
                    }
                    s += ":v";
                    s += eol(next(2));
                }
            }
            for d in (0..depth).rev() {
                s += &format!("END:C{d}");
                if d > 0 || next(2) == 0 {
                    s += eol(next(2));
                }
            }
            round_trip(&s);
        }
    }
}
