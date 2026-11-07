//! Contacts for the web app (phase 2 step 4): every address book the
//! person can see, searchable; one card read into its fields; and writes
//! through the core's lossless card editor with the same history (and
//! undo) as events.

use base64::Engine as _;
use calzone_core::card::{self, CardChange, CardError, CardView, Photo};
use calzone_core::edit::Outcome;
use chrono::NaiveDateTime;
use sqlx::PgPool;
use uuid::Uuid;

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

/// Every visible address book with its contacts (filtered by `q`, sorted
/// by name).
pub async fn list(pool: &PgPool, actor: Actor, q: &str) -> Result<Vec<Book>> {
    let mut out = Vec::new();
    for c in collections::visible(pool, actor, CollectionKind::AddressBook).await? {
        let mut contacts = Vec::new();
        for s in objects::all(pool, actor, c.id).await? {
            let Ok(v) = card::read(&s.body) else { continue };
            if !q.trim().is_empty() && !matches(&v, q) {
                continue;
            }
            let display = [v.name.trim(), v.org.trim()]
                .into_iter()
                .find(|x| !x.is_empty())
                .map(str::to_string)
                .or_else(|| v.emails.first().map(|e| e.value.clone()))
                .unwrap_or_else(|| "?".into());
            contacts.push(ContactRow {
                collection: c.id,
                name: s.meta.name,
                display,
                org: v.org.clone(),
                phone: v.phones.first().map(|p| p.value.clone()),
                email: v.emails.first().map(|e| e.value.clone()),
                has_photo: matches!(v.photo, Some(Photo::Inline { .. })),
            });
        }
        contacts.sort_by_key(|r| sort_key(&r.display));
        out.push(Book {
            id: c.id,
            name: book_name(&c),
            owner: c.owner_name.clone(),
            own: c.owner_id == actor,
            writable: c.access.can_write(),
            contacts,
        });
    }
    out.sort_by(|a, b| (!a.own, sort_key(&a.name)).cmp(&(!b.own, sort_key(&b.name))));
    Ok(out)
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
            phones: vec![Typed { kind: Kind::Mobile, value: "+44 7700 900001".into() }],
            emails: vec![Typed { kind: Kind::Home, value: "ada@example.org".into() }],
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
