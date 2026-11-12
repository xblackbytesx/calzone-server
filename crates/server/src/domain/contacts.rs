//! Contacts for the web app (phase 2 step 4): every address book the
//! person can see, searchable; one card read into its fields; and writes
//! through the core's lossless card editor with the same history (and
//! undo) as events. Clean-up (decision 56): has/lacks filters, likely
//! duplicates per address book, merges and bulk deletes as one change.

use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use calzone_core::card::{self, CardChange, CardError, CardView, Photo};
use calzone_core::dedupe::{self, Picks, Reason};
use calzone_core::edit::Outcome;
use chrono::NaiveDateTime;
use sqlx::PgPool;
use uuid::Uuid;

pub use super::events::Target;
use super::events::{self, Saved};
use super::{Actor, Collection, CollectionKind, DomainError, Result, collections, objects};

/// One contact in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactRow {
    pub collection: Uuid,
    pub name: String,
    /// The name shown (FN), else the organization, else an email.
    pub display: String,
    pub org: String,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub has_photo: bool,
}

/// One address book's contacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    pub id: Uuid,
    pub name: String,
    pub owner: String,
    pub own: bool,
    pub writable: bool,
    /// Groups of likely duplicates (books the person can change only).
    pub duplicates: usize,
    pub contacts: Vec<ContactRow>,
}

fn book_name(c: &Collection) -> String {
    if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() }
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Whether a card matches a search: any word of the query in its name,
/// organization, emails, notes, or (digits only) its phone numbers.
fn matches(v: &CardView, q: &str) -> bool {
    let list = |xs: &[calzone_core::card::Typed]| xs.iter().map(|e| e.value.as_str()).collect::<Vec<_>>().join(" ");
    let hay =
        format!("{} {} {} {} {} {}", v.name, v.given, v.family, v.org, list(&v.emails), list(&v.phones)).to_lowercase();
    let phones: Vec<String> = v.phones.iter().map(|p| digits(&p.value)).collect();
    q.split_whitespace().all(|w| {
        let w = w.to_lowercase();
        let d = digits(&w);
        hay.contains(&w)
            || (d.len() >= 3 && d.len() == w.trim_start_matches('+').len() && phones.iter().any(|p| p.contains(&d)))
    })
}

/// The sort key: names compare without case.
pub fn sort_key(s: &str) -> String {
    s.trim().to_lowercase()
}

/// One field's filter: has it, lacks it, or either (decision 56).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Want {
    #[default]
    Any,
    Has,
    Missing,
}

impl Want {
    pub fn parse(s: &str) -> Want {
        match s {
            "has" => Want::Has,
            "missing" => Want::Missing,
            _ => Want::Any,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Want::Any => "",
            Want::Has => "has",
            Want::Missing => "missing",
        }
    }

    fn keeps(self, has: bool) -> bool {
        match self {
            Want::Any => true,
            Want::Has => has,
            Want::Missing => !has,
        }
    }
}

/// What the contacts list shows: a search, one address book, and has or
/// lacks per field. Every part must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub q: String,
    pub book: Option<Uuid>,
    /// A first or last name (not only a shown name or an organization).
    pub name: Want,
    pub phone: Want,
    pub email: Want,
    pub address: Want,
    pub birthday: Want,
    pub photo: Want,
    pub org: Want,
}

impl Filter {
    /// How many field filters are set (the search and the book aside).
    pub fn fields_set(&self) -> usize {
        [self.name, self.phone, self.email, self.address, self.birthday, self.photo, self.org]
            .iter()
            .filter(|w| **w != Want::Any)
            .count()
    }

    pub fn keeps(&self, v: &CardView) -> bool {
        let filled = |s: &str| !s.trim().is_empty();
        (self.q.trim().is_empty() || matches(v, &self.q))
            && self.name.keeps(filled(&v.given) || filled(&v.family))
            && self.phone.keeps(v.phones.iter().any(|p| filled(&p.value)))
            && self.email.keeps(v.emails.iter().any(|e| filled(&e.value)))
            && self.address.keeps(v.addresses.iter().any(|a| !a.is_empty()))
            && self.birthday.keeps(v.birthday.is_some())
            && self.photo.keeps(v.photo.is_some())
            && self.org.keeps(filled(&v.org))
    }
}

fn row(collection: Uuid, name: String, v: &CardView) -> ContactRow {
    let display = [v.name.trim(), v.org.trim()]
        .into_iter()
        .find(|x| !x.is_empty())
        .map(str::to_string)
        .or_else(|| v.emails.first().map(|e| e.value.clone()))
        .unwrap_or_else(|| "?".into());
    ContactRow {
        collection,
        name,
        display,
        org: v.org.clone(),
        phone: v.phones.first().map(|p| p.value.clone()),
        email: v.emails.first().map(|e| e.value.clone()),
        has_photo: matches!(v.photo, Some(Photo::Inline { .. })),
    }
}

/// An address book's readable cards: name and fields.
struct Card {
    name: String,
    view: CardView,
}

async fn cards(pool: &PgPool, actor: Actor, collection: Uuid) -> Result<Vec<Card>> {
    Ok(objects::all(pool, actor, collection)
        .await?
        .into_iter()
        .filter_map(|s| {
            let view = card::read(&s.body).ok()?;
            Some(Card { name: s.meta.name, view })
        })
        .collect())
}

/// Every visible address book (or the one the filter names) with the
/// contacts the filter keeps, sorted by name, and how many groups of
/// likely duplicates each book the person can change holds.
pub async fn list(pool: &PgPool, actor: Actor, f: &Filter) -> Result<Vec<Book>> {
    let mut out = Vec::new();
    for c in collections::visible(pool, actor, CollectionKind::AddressBook).await? {
        if f.book.is_some_and(|b| b != c.id) {
            continue;
        }
        let all = cards(pool, actor, c.id).await?;
        let duplicates = if c.access.can_write() { find_groups(pool, c.id, &all).await?.len() } else { 0 };
        let mut contacts: Vec<ContactRow> =
            all.into_iter().filter(|k| f.keeps(&k.view)).map(|k| row(c.id, k.name, &k.view)).collect();
        contacts.sort_by_key(|r| sort_key(&r.display));
        out.push(Book {
            id: c.id,
            name: book_name(&c),
            owner: c.owner_name.clone(),
            own: c.owner_id == actor,
            writable: c.access.can_write(),
            duplicates,
            contacts,
        });
    }
    out.sort_by(|a, b| (!a.own, sort_key(&a.name)).cmp(&(!b.own, sort_key(&b.name))));
    Ok(out)
}

/// Every visible address book's id and name, for the filter's picker.
pub async fn books(pool: &PgPool, actor: Actor) -> Result<Vec<(Uuid, String)>> {
    let mut v: Vec<_> = collections::visible(pool, actor, CollectionKind::AddressBook)
        .await?
        .into_iter()
        .map(|c| (c.owner_id != actor, book_name(&c), c.id))
        .collect();
    v.sort_by(|a, b| (a.0, sort_key(&a.1)).cmp(&(b.0, sort_key(&b.1))));
    Ok(v.into_iter().map(|(_, n, id)| (id, n)).collect())
}

/// The pairs someone marked as different people in one book.
async fn apart(pool: &PgPool, collection: Uuid) -> Result<HashSet<(String, String)>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT a, b FROM contact_not_duplicates WHERE collection_id = $1")
            .bind(collection)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

fn pair(a: &str, b: &str) -> (String, String) {
    if a < b { (a.to_string(), b.to_string()) } else { (b.to_string(), a.to_string()) }
}

async fn find_groups(pool: &PgPool, collection: Uuid, all: &[Card]) -> Result<Vec<dedupe::Group>> {
    let apart = apart(pool, collection).await?;
    let views: Vec<CardView> = all.iter().map(|k| k.view.clone()).collect();
    Ok(dedupe::groups(&views, |i, j| apart.contains(&pair(&all[i].name, &all[j].name))))
}

/// One group of likely duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupGroup {
    pub cards: Vec<ContactRow>,
    pub reasons: Vec<Reason>,
}

/// One address book's likely duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupBook {
    pub id: Uuid,
    pub name: String,
    pub owner: String,
    pub own: bool,
    pub groups: Vec<DupGroup>,
}

/// Likely duplicates in every address book the person can change (only
/// those: merging changes the book). Groups sorted by their first name.
pub async fn duplicates(pool: &PgPool, actor: Actor) -> Result<Vec<DupBook>> {
    let mut out = Vec::new();
    for c in collections::visible(pool, actor, CollectionKind::AddressBook).await? {
        if !c.access.can_write() {
            continue;
        }
        let all = cards(pool, actor, c.id).await?;
        let mut groups: Vec<DupGroup> = find_groups(pool, c.id, &all)
            .await?
            .into_iter()
            .map(|g| {
                let mut cards: Vec<ContactRow> =
                    g.members.iter().map(|&i| row(c.id, all[i].name.clone(), &all[i].view)).collect();
                cards.sort_by_key(|r| sort_key(&r.display));
                DupGroup { cards, reasons: g.reasons }
            })
            .collect();
        // Strong matches (a phone or an email) before names alone.
        groups.sort_by_key(|g| (g.reasons == [Reason::Name], sort_key(&g.cards[0].display)));
        out.push(DupBook {
            id: c.id,
            name: book_name(&c),
            owner: c.owner_name.clone(),
            own: c.owner_id == actor,
            groups,
        });
    }
    out.sort_by(|a, b| (!a.own, sort_key(&a.name)).cmp(&(!b.own, sort_key(&b.name))));
    Ok(out)
}

async fn writable_book(pool: &PgPool, actor: Actor, collection: Uuid) -> Result<Collection> {
    let c = collections::get(pool, actor, collection).await?;
    if c.kind != CollectionKind::AddressBook {
        return Err(DomainError::WrongKind);
    }
    if !c.access.can_write() {
        return Err(DomainError::ReadOnly);
    }
    Ok(c)
}

/// Mark cards as different people: the finder stops pairing them. `apart`
/// false takes that back (the Undo after marking).
pub async fn set_apart(pool: &PgPool, actor: Actor, collection: Uuid, names: &[String], apart: bool) -> Result<()> {
    writable_book(pool, actor, collection).await?;
    let mut tx = pool.begin().await?;
    for (i, a) in names.iter().enumerate() {
        for b in &names[i + 1..] {
            if a == b {
                continue;
            }
            let (a, b) = pair(a, b);
            let sql = if apart {
                "INSERT INTO contact_not_duplicates (collection_id, a, b) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING"
            } else {
                "DELETE FROM contact_not_duplicates WHERE collection_id = $1 AND a = $2 AND b = $3"
            };
            sqlx::query(sql).bind(collection).bind(a).bind(b).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Cards about to be merged, in the order asked, as they are now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeSet {
    pub collection: Uuid,
    pub book: String,
    pub names: Vec<String>,
    pub etags: Vec<String>,
    pub cards: Vec<CardView>,
}

fn merge_size(n: usize) -> Result<()> {
    if (2..=dedupe::MAX_MERGE).contains(&n) {
        Ok(())
    } else {
        Err(DomainError::Invalid(format!("a merge takes 2 to {} contacts", dedupe::MAX_MERGE)))
    }
}

pub async fn merge_set(pool: &PgPool, actor: Actor, collection: Uuid, names: &[String]) -> Result<MergeSet> {
    let c = writable_book(pool, actor, collection).await?;
    let mut unique: Vec<String> = Vec::new();
    for n in names {
        if !unique.contains(n) {
            unique.push(n.clone());
        }
    }
    merge_size(unique.len())?;
    let mut set = MergeSet { collection, book: book_name(&c), names: Vec::new(), etags: Vec::new(), cards: Vec::new() };
    for n in unique {
        let s = objects::get(pool, actor, collection, &n).await?;
        let v = card::read(&s.body).map_err(|_| DomainError::NotFound)?;
        set.etags.push(s.meta.etag());
        set.names.push(n);
        set.cards.push(v);
    }
    Ok(set)
}

/// Merge the cards (`names` with the ETags the page saw) into the one at
/// `keep`, as one change: the kept card gets the picks and everything the
/// form doesn't cover, the others are deleted. Anything changed since the
/// page opened refuses it all (`PreconditionFailed`).
#[allow(clippy::too_many_arguments)]
pub async fn merge(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    names: &[String],
    etags: &[String],
    keep: usize,
    picks: &Picks,
    now: NaiveDateTime,
) -> Result<Uuid> {
    writable_book(pool, actor, collection).await?;
    merge_size(names.len())?;
    if etags.len() != names.len() || keep >= names.len() {
        return Err(DomainError::Invalid("the merge form is incomplete".into()));
    }
    let targets: Vec<Target> =
        names.iter().zip(etags).map(|(n, e)| Target { collection, name: n.clone(), etag: e.clone() }).collect();
    events::write_set(pool, actor, &targets, |texts| {
        let views: Vec<CardView> =
            texts.iter().map(|t| card::read(t).map_err(card_error)).collect::<Result<Vec<_>>>()?;
        let p = dedupe::proposal(&views, keep);
        let change = dedupe::change(&views, &p, picks);
        let others: Vec<&str> = texts.iter().enumerate().filter(|(i, _)| *i != keep).map(|(_, t)| t.as_str()).collect();
        let merged = dedupe::merge(&texts[keep], &others, &change, now).map_err(card_error)?;
        Ok((0..texts.len()).map(|i| (i == keep).then(|| merged.clone())).collect())
    })
    .await
}

/// A contact picked for a bulk action, as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    pub collection: Uuid,
    pub name: String,
    pub etag: String,
    pub display: String,
    pub book: String,
}

/// The most contacts one bulk delete takes.
pub const MAX_BULK: usize = 1000;

/// The picked contacts that still exist, in books the person can change;
/// others are left out.
pub async fn picked(pool: &PgPool, actor: Actor, refs: &[(Uuid, String)]) -> Result<Vec<Picked>> {
    let mut books: HashMap<Uuid, Option<String>> = HashMap::new();
    let mut out: Vec<Picked> = Vec::new();
    for (c, n) in refs.iter().take(MAX_BULK) {
        if out.iter().any(|p| p.collection == *c && p.name == *n) {
            continue;
        }
        if !books.contains_key(c) {
            let name = writable_book(pool, actor, *c).await.ok().map(|b| book_name(&b));
            books.insert(*c, name);
        }
        let Some(Some(book)) = books.get(c) else { continue };
        let Ok(s) = objects::get(pool, actor, *c, n).await else { continue };
        let Ok(v) = card::read(&s.body) else { continue };
        let r = row(*c, n.clone(), &v);
        out.push(Picked {
            collection: *c,
            name: n.clone(),
            etag: s.meta.etag(),
            display: r.display,
            book: book.clone(),
        });
    }
    out.sort_by_key(|p| sort_key(&p.display));
    Ok(out)
}

/// Delete the contacts under the ETags the page saw, as one change (one
/// Undo). Anything changed since refuses it all.
pub async fn remove_many(pool: &PgPool, actor: Actor, targets: &[Target]) -> Result<Uuid> {
    if targets.len() > MAX_BULK {
        return Err(DomainError::Invalid(format!("at most {MAX_BULK} contacts at once")));
    }
    let mut checked: HashSet<Uuid> = HashSet::new();
    for t in targets {
        if checked.insert(t.collection) {
            writable_book(pool, actor, t.collection).await?;
        }
    }
    events::write_set(pool, actor, targets, |texts| Ok(vec![None; texts.len()])).await
}

/// One contact as the page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactDetail {
    pub collection: Uuid,
    pub name: String,
    pub etag: String,
    pub card: CardView,
    pub book: String,
    pub writable: bool,
}

pub async fn detail(pool: &PgPool, actor: Actor, collection: Uuid, name: &str) -> Result<ContactDetail> {
    let c = collections::get(pool, actor, collection).await?;
    if c.kind != CollectionKind::AddressBook {
        return Err(DomainError::NotFound);
    }
    let s = objects::get(pool, actor, collection, name).await?;
    let card = card::read(&s.body).map_err(|_| DomainError::NotFound)?;
    Ok(ContactDetail {
        collection,
        name: name.to_string(),
        etag: s.meta.etag(),
        card,
        book: book_name(&c),
        writable: c.access.can_write(),
    })
}

/// The photo's bytes and media type, for inline photos of common image
/// types only (links are never fetched).
pub async fn photo(pool: &PgPool, actor: Actor, collection: Uuid, name: &str) -> Result<(String, Vec<u8>, String)> {
    let d = detail(pool, actor, collection, name).await?;
    match d.card.photo {
        Some(Photo::Inline { mime, base64 })
            if matches!(mime.as_str(), "image/jpeg" | "image/jpg" | "image/png" | "image/gif" | "image/webp") =>
        {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(base64.as_bytes())
                .or_else(|_| {
                    base64::engine::general_purpose::STANDARD_NO_PAD.decode(base64.trim_end_matches('=').as_bytes())
                })
                .map_err(|_| DomainError::NotFound)?;
            let mime = if mime == "image/jpg" { "image/jpeg".into() } else { mime };
            Ok((mime, bytes, d.etag))
        }
        _ => Err(DomainError::NotFound),
    }
}

fn card_error(e: CardError) -> DomainError {
    DomainError::Invalid(e.to_string())
}

pub async fn create(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    change: &CardChange,
    now: NaiveDateTime,
) -> Result<Saved> {
    let c = collections::get(pool, actor, collection).await?;
    if c.kind != CollectionKind::AddressBook {
        return Err(DomainError::WrongKind);
    }
    let uid = Uuid::new_v4().to_string();
    let text = card::new_card(change, &uid, now).map_err(card_error)?;
    events::create_object(pool, actor, collection, &format!("{uid}.vcf"), &text).await
}

/// Change a contact under the ETag the page saw; a newer edit elsewhere is
/// refused (`PreconditionFailed`), never overwritten.
pub async fn update(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    name: &str,
    etag: &str,
    change: &CardChange,
    now: NaiveDateTime,
) -> Result<Saved> {
    events::write(pool, actor, collection, name, etag, |text| {
        let updated = card::apply(text, change, now).map_err(card_error)?;
        Ok(Outcome { updated: Some(updated), created: None })
    })
    .await
}

pub async fn remove(pool: &PgPool, actor: Actor, collection: Uuid, name: &str, etag: &str) -> Result<Saved> {
    events::write(pool, actor, collection, name, etag, |_| Ok(Outcome { updated: None, created: None })).await
}

/// Address books the person can add to, their own first.
pub async fn writable_books(pool: &PgPool, actor: Actor) -> Result<Vec<(Uuid, String)>> {
    let mut v: Vec<_> = collections::visible(pool, actor, CollectionKind::AddressBook)
        .await?
        .into_iter()
        .filter(|c| c.access.can_write())
        .map(|c| (c.owner_id != actor, book_name(&c), c.id))
        .collect();
    v.sort_by(|a, b| (a.0, sort_key(&a.1)).cmp(&(b.0, sort_key(&b.1))));
    Ok(v.into_iter().map(|(_, n, id)| (id, n)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use calzone_core::card::{Kind, Typed};

    #[test]
    fn search() {
        let v = CardView {
            name: "Ada Lovelace".into(),
            org: "Analytical".into(),
            phones: vec![Typed::new(Kind::Mobile, "+44 7700 900001")],
            emails: vec![Typed::new(Kind::Home, "ada@example.org")],
            ..Default::default()
        };
        for q in ["ada", "LOVE", "ada love", "analyt", "example.org", "7700", "+44 7700", "900001"] {
            assert!(matches(&v, q), "{q}");
        }
        for q in ["bob", "ada bob", "12", "9999"] {
            assert!(!matches(&v, q), "{q}");
        }
    }
}
