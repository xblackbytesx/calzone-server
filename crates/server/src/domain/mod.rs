//! The domain service (decision 2): the only code that reads or writes the
//! store. The web app, the sync API, the DAV door and the MCP door all call
//! these functions; none of them touch the database.
//!
//! Every call names its actor (a principal id) and checks access first.
//! Anything the actor may not see is `NotFound`, never a hint that it exists
//! (AGENTS.md invariants).

pub mod access;
pub mod accounts;
pub mod app_passwords;
pub mod birthdays;
pub mod collections;
pub mod contacts;
pub mod devices;
pub mod events;
pub mod feeds;
pub mod imports;
pub mod objects;
pub mod prefs;
pub mod principals;
pub mod subscriptions;
pub mod sync;
pub mod two_factor;

pub use access::Access;
pub use collections::{Collection, CollectionKind, NewCollection};
pub use objects::{Changes, ObjectMeta, Precondition, Served, Written};

/// A principal id: who is acting.
pub type Actor = uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    /// Missing, or invisible to this actor (indistinguishable on purpose).
    #[error("not found")]
    NotFound,
    /// Visible, but the actor may not change it.
    #[error("read-only for you")]
    ReadOnly,
    /// `If-Match` / `If-None-Match` did not hold.
    #[error("precondition failed")]
    PreconditionFailed,
    /// Another resource in the collection has this UID (CalDAV `no-uid-conflict`).
    #[error("UID already used by {existing}")]
    UidConflict { existing: String },
    /// Not a valid object (parse error or resource rules).
    #[error("invalid object: {0}")]
    Invalid(String),
    /// A card in a calendar, or an event in an address book.
    #[error("wrong kind of object for this collection")]
    WrongKind,
    /// The calendar does not accept this component type.
    #[error("component type not accepted by this calendar")]
    ComponentNotAccepted,
    /// A resource or collection name that cannot be used.
    #[error("invalid name")]
    BadName,
    /// A name that is already taken (collection slug, principal name).
    #[error("name already taken")]
    NameTaken,
    /// A sync token this collection never issued.
    #[error("invalid sync token")]
    BadSyncToken,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub type Result<T> = std::result::Result<T, DomainError>;

/// Resource names: 1 to 255 bytes, no `/`, no control characters, not `.`
/// or `..`.
pub fn valid_resource_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.chars().any(char::is_control)
}

/// Principal names and collection slugs: 1 to 64 of `a-z 0-9 . _ -`, not
/// starting with `.`.
pub fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_resource_name("abc-123.ics"));
        assert!(valid_resource_name("é ü.vcf"));
        assert!(!valid_resource_name(""));
        assert!(!valid_resource_name(".."));
        assert!(!valid_resource_name("a/b.ics"));
        assert!(!valid_resource_name("a\nb"));
        assert!(!valid_resource_name(&"x".repeat(256)));
        assert!(valid_slug("family-2026"));
        assert!(!valid_slug("Family"));
        assert!(!valid_slug(".hidden"));
        assert!(!valid_slug("a b"));
    }
}
