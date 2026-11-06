//! Exports (decision 30): one collection as `.ics` / `.vcf`, or everything a
//! person can reach as one zip. Objects are served as that person reads
//! them, so their own alarms are merged back in and nobody else's are.

use std::io::Write;

use calzone_core::import::{assemble_calendar, assemble_cards};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{Actor, Collection, CollectionKind, DomainError, collections, objects};

/// One collection as a file: (file name, content type, text).
pub async fn collection(pool: &PgPool, actor: Actor, id: Uuid) -> Result<(String, &'static str, String), DomainError> {
    let c = collections::get(pool, actor, id).await?;
    let text = render(pool, actor, &c).await?;
    let (ext, ct) = ext(c.kind);
    Ok((format!("{}.{ext}", c.slug), ct, text))
}

fn ext(kind: CollectionKind) -> (&'static str, &'static str) {
    match kind {
        CollectionKind::Calendar => ("ics", "text/calendar; charset=utf-8"),
        CollectionKind::AddressBook => ("vcf", "text/vcard; charset=utf-8"),
    }
}

async fn render(pool: &PgPool, actor: Actor, c: &Collection) -> Result<String, DomainError> {
    let bodies: Vec<String> = objects::all(pool, actor, c.id).await?.into_iter().map(|s| s.body).collect();
    Ok(match c.kind {
        CollectionKind::Calendar => {
            let name = if c.display_name.is_empty() { &c.slug } else { &c.display_name };
            let (text, broken) = assemble_calendar(name, &bodies);
            if broken > 0 {
                tracing::warn!(collection = %c.id, broken, "export: stored objects that do not parse were left out");
            }
            text
        }
        CollectionKind::AddressBook => assemble_cards(&bodies),
    })
}

/// Everything the actor can reach, as a zip: their own collections at the
/// top (`family.ics`, `contacts.vcf`), shared ones under
/// `shared/<owner>/`.
pub async fn everything(pool: &PgPool, actor: Actor) -> Result<Vec<u8>, DomainError> {
    let mut all = collections::visible(pool, actor, CollectionKind::Calendar).await?;
    all.extend(collections::visible(pool, actor, CollectionKind::AddressBook).await?);
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for c in &all {
            let text = render(pool, actor, c).await?;
            let (ext, _) = ext(c.kind);
            let path = if c.owner_id == actor {
                format!("{}.{ext}", c.slug)
            } else {
                format!("shared/{}/{}.{ext}", c.owner_name, c.slug)
            };
            z.start_file(path, opts).map_err(zip_error)?;
            z.write_all(text.as_bytes()).map_err(|e| zip_error(e.into()))?;
        }
        z.finish().map_err(zip_error)?;
    }
    Ok(buf.into_inner())
}

fn zip_error(e: zip::result::ZipError) -> DomainError {
    // Writing to memory only fails on a bug; surface it as a server error.
    DomainError::Db(sqlx::Error::Protocol(format!("zip: {e}")))
}
