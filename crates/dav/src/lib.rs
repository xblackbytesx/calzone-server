//! CalZone's CalDAV/CardDAV door (decision 3): the protocol half.
//!
//! Pure, like the core: no IO, no database. It parses request paths and
//! bodies, evaluates query filters with the core, and writes multistatus
//! XML. The server's `dav` module does auth and calls the domain service
//! with what this crate parsed (decision 28).

pub mod filter;
pub mod path;
pub mod request;
pub mod response;
pub mod xml;

/// The `DAV` header for OPTIONS.
pub const DAV_HEADER: &str = "1, 3, access-control, calendar-access, addressbook, extended-mkcol";

/// Methods the door answers.
pub const ALLOW: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, REPORT, MKCALENDAR, MKCOL";

/// Largest object accepted by PUT (`max-resource-size`): room for a vCard
/// with a large photo.
pub const MAX_RESOURCE_SIZE: usize = 10 * 1024 * 1024;

/// Largest XML request body (a multigets for thousands of hrefs fits).
pub const MAX_XML_SIZE: usize = 4 * 1024 * 1024;

/// Sync tokens are URIs (RFC 6578) naming the collection and its sequence,
/// so a token from a deleted and recreated collection is never accepted.
pub fn sync_token(collection: &str, sequence: i64) -> String {
    format!("urn:calzone:sync:{collection}:{sequence}")
}

/// The sequence in a token we issued for this collection.
pub fn parse_sync_token(collection: &str, token: &str) -> Option<i64> {
    let rest = token.trim().strip_prefix("urn:calzone:sync:")?;
    let (id, seq) = rest.rsplit_once(':')?;
    if id != collection {
        return None;
    }
    seq.parse().ok().filter(|s| *s >= 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_tokens() {
        let t = sync_token("abc", 7);
        assert_eq!(parse_sync_token("abc", &t), Some(7));
        assert_eq!(parse_sync_token("abd", &t), None, "another collection's token");
        assert_eq!(parse_sync_token("abc", "7"), None);
        assert_eq!(parse_sync_token("abc", "urn:calzone:sync:abc:-1"), None);
        assert_eq!(parse_sync_token("abc", "urn:calzone:sync:abc:x"), None);
    }
}
