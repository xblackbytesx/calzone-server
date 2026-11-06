//! Imports (decision 30): files (`.ics`, `.vcf`, a Google Takeout zip) and
//! a Baikal account read over CalDAV/CardDAV. Both go through
//! [`objects::upsert`], so the importing account's alarms land in its own
//! layer and a re-run changes nothing. Every object that cannot be stored is
//! kept in the report as the bytes received.
//!
//! An import is started with [`Run::begin`] (the report exists from then on)
//! and driven by [`Run::files`] or [`Run::baikal`]; the web app runs those
//! in a background task and the report page follows it.

pub mod baikal;
mod digest;

use std::collections::{HashMap, HashSet};
use std::io::Read;

use calzone_core::import::{self as files, FileError};
use calzone_core::lines::Document;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::imports::{self, CollectionCounts, Failure, Status};
use crate::domain::objects::{self, Upserted};
use crate::domain::{Collection, CollectionKind, DomainError, NewCollection, collections, principals};

/// Upload limits: the request body, and what a zip may expand to.
pub const MAX_UPLOAD: usize = 64 * 1024 * 1024;
const MAX_ZIP_TOTAL: u64 = 256 * 1024 * 1024;
const MAX_ZIP_FILES: usize = 1000;

/// Where uploaded files go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Each file into the collection named after it (its `X-WR-CALNAME`, or
    /// the file name), made when the account has none of that name and kind.
    /// Importing the same file again lands in the same place.
    ByName,
    /// Everything into this collection.
    Existing(Uuid),
}

#[derive(Debug, Clone)]
pub struct Upload {
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// One running import and its report.
pub struct Run {
    pool: PgPool,
    pub id: Uuid,
    /// The account the objects go into.
    into: Uuid,
    into_name: String,
    summary: Vec<CollectionCounts>,
    /// UIDs written by this run, per collection: a second object with the
    /// same UID would overwrite the first, so it is kept as a failure.
    seen: HashMap<Uuid, HashSet<String>>,
    failures: i32,
    since_progress: u32,
}

/// Something that stops the whole import (the database, an unreachable
/// source); per-object problems are failures in the report instead.
#[derive(Debug, thiserror::Error)]
pub enum Stop {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Domain(#[from] DomainError),
}

impl Run {
    /// Start the report for an import into `into`, started by `by`.
    /// `source` is shown on the report and must hold no credential.
    pub async fn begin(pool: &PgPool, into: Uuid, by: Uuid, source: &str) -> Result<Self, DomainError> {
        let into_name = principals::get(pool, into).await?.name;
        let id = imports::start(pool, into, by, source).await?;
        Ok(Self {
            pool: pool.clone(),
            id,
            into,
            into_name,
            summary: Vec::new(),
            seen: HashMap::new(),
            failures: 0,
            since_progress: 0,
        })
    }

    /// Close the report.
    async fn finish(self, outcome: Result<(), Stop>) -> Uuid {
        let (status, message) = match outcome {
            Ok(()) => (Status::Done, String::new()),
            Err(e) => (Status::Failed, e.to_string()),
        };
        if let Err(e) = imports::finish(&self.pool, self.id, status, &message, &self.summary).await {
            tracing::warn!(import = %self.id, error = %e, "could not close an import report");
        }
        self.id
    }

    /// The index of a collection in the summary, added when new.
    fn slot(&mut self, c: &Collection, created: bool) -> usize {
        if let Some(i) = self.summary.iter().position(|s| s.collection_id == Some(c.id)) {
            return i;
        }
        self.summary.push(CollectionCounts {
            collection_id: Some(c.id),
            name: display(c),
            created_collection: created,
            ..Default::default()
        });
        self.summary.len() - 1
    }

    /// A failure that belongs to no collection (a file that is not text, a
    /// zip that does not open). Counted under that file's name.
    async fn fail_file(&mut self, file: &str, reason: String, raw: &[u8]) -> Result<(), Stop> {
        let i = match self.summary.iter().position(|s| s.collection_id.is_none() && s.name == file) {
            Some(i) => i,
            None => {
                self.summary.push(CollectionCounts { name: file.to_string(), ..Default::default() });
                self.summary.len() - 1
            }
        };
        self.fail(i, reason, raw).await
    }

    async fn fail(&mut self, slot: usize, reason: String, raw: &[u8]) -> Result<(), Stop> {
        self.summary[slot].failed += 1;
        self.failures += 1;
        let f = Failure { collection: self.summary[slot].name.clone(), reason, raw: raw.to_vec() };
        imports::add_failure(&self.pool, self.id, self.failures, &f).await?;
        Ok(())
    }

    /// Store one object; `name` is used when no object has its UID yet.
    async fn object(&mut self, slot: usize, collection: Uuid, name: &str, body: &str) -> Result<(), Stop> {
        let uid = uid_of(body);
        if let Some(u) = &uid
            && self.seen.get(&collection).is_some_and(|s| s.contains(u))
        {
            let why = "the same UID appears twice in this import; the first one was kept".to_string();
            return self.fail(slot, why, body.as_bytes()).await;
        }
        match objects::upsert(&self.pool, self.into, collection, name, body).await {
            Ok((how, _)) => {
                let s = &mut self.summary[slot];
                match how {
                    Upserted::Created => s.created += 1,
                    Upserted::Updated => s.updated += 1,
                    Upserted::Unchanged => s.unchanged += 1,
                }
                if let Some(u) = uid {
                    self.seen.entry(collection).or_default().insert(u);
                }
            }
            Err(DomainError::Db(e)) => return Err(DomainError::Db(e).into()),
            Err(e) => self.fail(slot, reason(&e), body.as_bytes()).await?,
        }
        self.since_progress += 1;
        if self.since_progress >= 50 {
            self.since_progress = 0;
            imports::progress(&self.pool, self.id, &self.summary).await?;
        }
        Ok(())
    }

    /// The account's own collection with this slug and kind, or a new one.
    async fn collection_named(
        &mut self,
        kind: CollectionKind,
        slug: &str,
        new: NewCollection<'_>,
    ) -> Result<(Collection, bool), Stop> {
        match collections::by_path(&self.pool, self.into, &self.into_name.clone(), slug).await {
            Ok(c) if c.kind == kind && c.owner_id == self.into => return Ok((c, false)),
            Ok(_) | Err(DomainError::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        let c = collections::create_numbered(&self.pool, self.into, slug, new).await?;
        Ok((c, true))
    }

    /// Import uploaded files and close the report; returns its id.
    pub async fn files(mut self, uploads: Vec<Upload>, target: Target) -> Uuid {
        let outcome = self.files_inner(uploads, target).await;
        self.finish(outcome).await
    }

    async fn files_inner(&mut self, uploads: Vec<Upload>, target: Target) -> Result<(), Stop> {
        let existing = match target {
            Target::Existing(id) => {
                let c = collections::get(&self.pool, self.into, id).await?;
                if !c.access.can_write() {
                    return Err(Stop::Message(format!("{} is read-only for this account", display(&c))));
                }
                Some(c)
            }
            Target::ByName => None,
        };
        let mut queue = Vec::new();
        for u in uploads {
            if is_zip(&u.bytes) {
                match unzip(&u.bytes) {
                    Ok(inner) if inner.is_empty() => {
                        self.fail_file(&u.filename, "the zip holds no .ics or .vcf file".into(), b"").await?
                    }
                    Ok(inner) => queue.extend(inner),
                    Err(why) => self.fail_file(&u.filename, why, b"").await?,
                }
            } else {
                queue.push(u);
            }
        }
        for u in queue {
            self.file(u, existing.as_ref()).await?;
        }
        Ok(())
    }

    async fn file(&mut self, u: Upload, existing: Option<&Collection>) -> Result<(), Stop> {
        let text = match files::decode(&u.bytes) {
            Ok((t, _repaired)) => t,
            Err(e) => return self.fail_file(&u.filename, e.to_string(), &u.bytes).await,
        };
        let kind = match sniff(&text) {
            Some(k) => k,
            None => {
                let why = "not a calendar (.ics) or contacts (.vcf) file".to_string();
                return self.fail_file(&u.filename, why, &u.bytes).await;
            }
        };
        let (objects, title) = match kind {
            CollectionKind::Calendar => match files::split_calendar(&text) {
                Ok(f) => (f.objects, f.name),
                Err(e) => return self.fail_file(&u.filename, file_reason(&e), &u.bytes).await,
            },
            CollectionKind::AddressBook => match files::split_cards(&text) {
                Ok(cards) => (cards, None),
                Err(e) => return self.fail_file(&u.filename, file_reason(&e), &u.bytes).await,
            },
        };
        let (c, created) = match existing {
            Some(c) if c.kind != kind => {
                let why = format!(
                    "this is {} file, but {} is {}",
                    if kind == CollectionKind::Calendar { "a calendar" } else { "a contacts" },
                    display(c),
                    if c.kind == CollectionKind::Calendar { "a calendar" } else { "an address book" },
                );
                return self.fail_file(&u.filename, why, &u.bytes).await;
            }
            Some(c) => (c.clone(), false),
            None => {
                let name = title.unwrap_or_else(|| stem(&u.filename));
                let fallback = if kind == CollectionKind::Calendar { "calendar" } else { "contacts" };
                let slug = collections::slugify(&name, fallback);
                let new = NewCollection {
                    kind,
                    slug: "",
                    display_name: &name,
                    description: "",
                    color: None,
                    components: &[],
                };
                self.collection_named(kind, &slug, new).await?
            }
        };
        let slot = self.slot(&c, created);
        let ext = if kind == CollectionKind::Calendar { "ics" } else { "vcf" };
        for body in objects {
            let name = resource_name(&body, ext);
            self.object(slot, c.id, &name, &body).await?;
        }
        Ok(())
    }
}

fn display(c: &Collection) -> String {
    if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() }
}

/// What the report says about an object the store refused.
fn reason(e: &DomainError) -> String {
    match e {
        DomainError::Invalid(why) => format!("not a valid object: {why}"),
        DomainError::WrongKind => "a contact in a calendar, or an event in an address book".into(),
        DomainError::ComponentNotAccepted => "this calendar does not take this kind of entry".into(),
        DomainError::BadName => "no usable name".into(),
        DomainError::UidConflict { .. } => "its UID is already used by another entry".into(),
        other => other.to_string(),
    }
}

fn file_reason(e: &FileError) -> String {
    match e {
        FileError::Parse(p) => format!("the file does not parse: {p}"),
        FileError::WrongKind(_) => "the file mixes calendar and contacts data".into(),
    }
}

/// Calendar or contacts, from the first component.
fn sniff(text: &str) -> Option<CollectionKind> {
    let head = text.trim_start_matches('\u{feff}').trim_start();
    let upper = head.get(..16).unwrap_or(head).to_ascii_uppercase();
    if upper.starts_with("BEGIN:VCALENDAR") {
        Some(CollectionKind::Calendar)
    } else if upper.starts_with("BEGIN:VCARD") {
        Some(CollectionKind::AddressBook)
    } else {
        None
    }
}

fn stem(filename: &str) -> String {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    let s = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base).trim();
    if s.is_empty() { "Imported".into() } else { s.chars().take(100).collect() }
}

/// The object's UID, if it has one.
fn uid_of(body: &str) -> Option<String> {
    let doc = Document::parse(body).ok()?;
    let top = doc.components.first()?;
    let uid = match top.property("UID") {
        Some(l) => l.value().to_string(),
        None => top.components().find_map(|c| c.property("UID")).map(|l| l.value().to_string())?,
    };
    (!uid.is_empty()).then_some(uid)
}

/// A stable resource name: the UID when it is short and plain, else a hash
/// of it (or of the body, for a card without UID).
pub fn resource_name(body: &str, ext: &str) -> String {
    match uid_of(body) {
        Some(u)
            if u.len() <= 100
                && u.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
                && !u.starts_with('.') =>
        {
            format!("{u}.{ext}")
        }
        Some(u) => format!("{}.{ext}", hex32(u.as_bytes())),
        None => format!("{}.{ext}", hex32(body.as_bytes())),
    }
}

fn hex32(b: &[u8]) -> String {
    Sha256::digest(b).iter().take(16).map(|x| format!("{x:02x}")).collect()
}

fn is_zip(b: &[u8]) -> bool {
    b.starts_with(b"PK\x03\x04")
}

/// The `.ics` and `.vcf` files in a zip (a Google Takeout export keeps one
/// `.ics` per calendar under `Takeout/Calendar/`). Bounded, so a small zip
/// cannot expand into gigabytes.
fn unzip(bytes: &[u8]) -> Result<Vec<Upload>, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| "the zip does not open".to_string())?;
    if z.len() > MAX_ZIP_FILES * 10 {
        return Err("the zip holds too many files".into());
    }
    let mut out = Vec::new();
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|_| "the zip does not open".to_string())?;
        let name = f.name().to_string();
        let lower = name.to_ascii_lowercase();
        if f.is_dir() || !(lower.ends_with(".ics") || lower.ends_with(".vcf")) {
            continue;
        }
        if out.len() >= MAX_ZIP_FILES {
            return Err("the zip holds too many calendar or contacts files".into());
        }
        let mut buf = Vec::new();
        let budget = MAX_ZIP_TOTAL - total;
        (&mut f).take(budget + 1).read_to_end(&mut buf).map_err(|_| format!("{name} in the zip does not unpack"))?;
        total += buf.len() as u64;
        if total > MAX_ZIP_TOTAL {
            return Err("the zip unpacks to more than 256 MiB".into());
        }
        out.push(Upload { filename: name, bytes: buf });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_uids() {
        let cal =
            |uid: &str| format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n");
        assert_eq!(resource_name(&cal("abc-1@google.com"), "ics"), "abc-1@google.com.ics");
        let odd = resource_name(&cal("a/b c"), "ics");
        assert_eq!(odd.len(), 36);
        assert_eq!(odd, resource_name(&cal("a/b c"), "ics"));
        let card = "BEGIN:VCARD\r\nFN:x\r\nEND:VCARD\r\n";
        assert_eq!(resource_name(card, "vcf").len(), 36);
        assert_eq!(resource_name("BEGIN:VCARD\r\nUID:u1\r\nEND:VCARD\r\n", "vcf"), "u1.vcf");
    }

    #[test]
    fn kinds_and_stems() {
        assert_eq!(sniff("\u{feff}\r\nbegin:vcalendar\r\n"), Some(CollectionKind::Calendar));
        assert_eq!(sniff("BEGIN:VCARD"), Some(CollectionKind::AddressBook));
        assert_eq!(sniff("<html>"), None);
        assert_eq!(stem("Takeout/Calendar/Family.ics"), "Family");
        assert_eq!(stem("C:\\x\\work.vcf"), "work");
        assert_eq!(stem(".ics"), "Imported");
    }

    #[test]
    fn zips_are_bounded_and_filtered() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            w.start_file("Takeout/Calendar/Family.ics", o).unwrap();
            w.write_all(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
            w.start_file("Takeout/archive_browser.html", o).unwrap();
            w.write_all(b"<html>").unwrap();
            w.finish().unwrap();
        }
        let bytes = buf.into_inner();
        assert!(is_zip(&bytes));
        let files = unzip(&bytes).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "Takeout/Calendar/Family.ics");
        assert!(unzip(b"PK\x03\x04garbage").is_err());
    }
}
