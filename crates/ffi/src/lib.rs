//! calzone-ffi: the Android app's window into the phone's store and sync
//! engine (`calzone-client`), through UniFFI (decision 44).
//!
//! Thin, JSON in and out: collections, objects, conflicts, reports and the
//! account cross as the core's and the client's own serde shapes, the same
//! ones `/sync/v1` sends, so the app decodes one set of models. Real
//! failures are a flat error per kind (`CzException.Network`, `.SignedOut`,
//! ...), so the app can tell "try later" from "sign in again".
//!
//! HTTP is the app's: it implements [`HttpTransport`] with OkHttp, adding
//! the server's address and the device token. Calls block; the app makes
//! them off the main thread (the sync adapter's own thread, WorkManager).

use std::sync::Arc;

use calzone_client::{Error, Request, Response, Store, Transport, bridge, engine, link};
use serde::Serialize;

uniffi::setup_scaffolding!();

/// Flat: Kotlin gets the Display text as the exception message. Variant
/// fields named `message` would clash with `Throwable.message` in Kotlin.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum CzError {
    /// No answer from the server: try again later, nothing was lost.
    #[error("{message}")]
    Network { message: String },
    /// The server no longer knows this phone: sign in again.
    #[error("{message}")]
    SignedOut { message: String },
    /// An answer the app did not expect.
    #[error("{message}")]
    Server { message: String },
    /// The app asked for something that cannot be done: a bug, or an
    /// object that does not parse.
    #[error("{message}")]
    Input { message: String },
    /// The phone's database failed.
    #[error("{message}")]
    Store { message: String },
}

impl From<Error> for CzError {
    fn from(e: Error) -> Self {
        let message = e.to_string();
        match e {
            Error::Network(_) => CzError::Network { message },
            Error::SignedOut => CzError::SignedOut { message },
            Error::Server { .. } => CzError::Server { message },
            Error::Input(_) => CzError::Input { message },
            Error::Store(_) => CzError::Store { message },
        }
    }
}

fn parse_row(row: &str) -> Result<calzone_core::android::EventRow, CzError> {
    serde_json::from_str(row).map_err(|e| CzError::Input { message: format!("not an Android row: {e}") })
}

fn json(v: &impl Serialize) -> String {
    serde_json::to_string(v).expect("client types serialize")
}

/// The answer to one request. `error` set means there was no answer (no
/// network, a timeout, TLS); `status` is then 0.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct HttpResult {
    pub status: u16,
    pub body: String,
    pub error: Option<String>,
}

/// The app's HTTP client. `method` is `GET`, `POST`, `PUT` or `DELETE`;
/// `path` starts with `/sync/v1` and goes after the server's address;
/// `body`, when present, is JSON. Once signed in, it sends
/// `Authorization: Bearer <token>`.
#[uniffi::export(with_foreign)]
pub trait HttpTransport: Send + Sync {
    fn send(&self, method: String, path: String, body: Option<String>) -> HttpResult;
}

struct Foreign<'a>(&'a dyn HttpTransport);

impl Transport for Foreign<'_> {
    fn send(&self, r: &Request) -> Result<Response, String> {
        let a = self.0.send(r.method.as_str().to_string(), r.path.clone(), r.body.clone());
        match a.error {
            Some(e) => Err(e),
            None => Ok(Response { status: a.status, body: a.body }),
        }
    }
}

/// A sign-in address taken apart.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SignInLink {
    /// `https://calzone.example.com`: the transport's base address.
    pub server: String,
    pub code: String,
}

/// The QR code's (or a pasted) address, or `None` when it is not a
/// CalZone sign-in address.
#[uniffi::export]
pub fn parse_link(url: String) -> Option<SignInLink> {
    link::parse(&url).map(|(server, code)| SignInLink { server, code })
}

/// The phone's store and its sync engine. One per process; every method
/// is safe to call from any thread (the store serializes them).
#[derive(uniffi::Object)]
pub struct CalZone {
    store: Store,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[uniffi::export]
impl CalZone {
    /// Open (or create) the store at `path`, in the app's own files.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Arc<CalZone>, CzError> {
        Ok(Arc::new(CalZone { store: Store::open(path)? }))
    }

    // --- the account ---

    /// Redeem a sign-in code ([`parse_link`]) on `server`. Returns
    /// `SignedIn` JSON: the token (keep it in the account manager; it is
    /// shown only here) and the account.
    pub fn sign_in(
        &self,
        transport: Arc<dyn HttpTransport>,
        server: String,
        code: String,
        device: String,
    ) -> Result<String, CzError> {
        Ok(json(&engine::sign_in(&self.store, &Foreign(transport.as_ref()), &server, &code, &device)?))
    }

    /// The stored `Account` JSON, or `None` before sign-in.
    pub fn account(&self) -> Result<Option<String>, CzError> {
        Ok(self.store.account()?.map(|a| json(&a)))
    }

    /// The server's address this store belongs to.
    pub fn server(&self) -> Result<Option<String>, CzError> {
        Ok(self.store.server()?)
    }

    /// Fetch the account from the server (`Account` JSON) and store it.
    pub fn refresh_account(&self, transport: Arc<dyn HttpTransport>) -> Result<String, CzError> {
        Ok(json(&engine::refresh_account(&self.store, &Foreign(transport.as_ref()))?))
    }

    /// Sign out: the server forgets this phone (if it can be reached) and
    /// the store is wiped, edits not yet sent included.
    pub fn sign_out(&self, transport: Arc<dyn HttpTransport>) -> Result<(), CzError> {
        Ok(engine::sign_out(&self.store, &Foreign(transport.as_ref()))?)
    }

    pub fn register_push(
        &self,
        transport: Arc<dyn HttpTransport>,
        endpoint: String,
        key: String,
        auth: String,
    ) -> Result<(), CzError> {
        Ok(engine::register_push(&Foreign(transport.as_ref()), &endpoint, &key, &auth)?)
    }

    pub fn unregister_push(&self, transport: Arc<dyn HttpTransport>) -> Result<(), CzError> {
        Ok(engine::unregister_push(&Foreign(transport.as_ref()))?)
    }

    // --- syncing ---

    /// One sync run; a `Report` JSON. The app runs one at a time.
    pub fn sync(&self, transport: Arc<dyn HttpTransport>) -> Result<String, CzError> {
        Ok(json(&engine::sync(&self.store, &Foreign(transport.as_ref()), now())?))
    }

    // --- reading ---

    /// `[StoredCollection]` JSON.
    pub fn collections(&self) -> Result<String, CzError> {
        Ok(json(&self.store.collections()?))
    }

    /// `[ObjectMeta]` JSON for one collection.
    pub fn objects(&self, collection: String) -> Result<String, CzError> {
        Ok(json(&self.store.objects(&collection)?))
    }

    /// `StoredObject` JSON (the full text), or `None`.
    pub fn object(&self, collection: String, name: String) -> Result<Option<String>, CzError> {
        Ok(self.store.object(&collection, &name)?.map(|o| json(&o)))
    }

    /// `[ObjectMeta]` JSON: calendar objects that may occur in
    /// `[from, to)` (UTC seconds).
    pub fn in_range(&self, from: i64, to: i64) -> Result<String, CzError> {
        Ok(json(&self.store.in_range(from, to)?))
    }

    /// `[Conflict]` JSON: what to tell the person.
    pub fn conflicts(&self) -> Result<String, CzError> {
        Ok(json(&self.store.conflicts()?))
    }

    pub fn dismiss_conflict(&self, id: i64) -> Result<(), CzError> {
        Ok(self.store.dismiss_conflict(id)?)
    }

    /// `[LocalChange]` JSON after `after`: what the bridge must mirror.
    pub fn local_changes(&self, after: i64, limit: u32) -> Result<String, CzError> {
        Ok(json(&self.store.local_changes(after, limit)?))
    }

    /// The bridge has applied everything up to `upto`.
    pub fn forget_local_changes(&self, upto: i64) -> Result<(), CzError> {
        Ok(self.store.forget_local_changes(upto)?)
    }

    // --- the calendar bridge (decision 49) ---

    /// `EventRows` JSON for a stored object in the phone's zone (an IANA
    /// name), or `None` when it is gone or not an event.
    pub fn event_rows(&self, collection: String, name: String, phone_zone: String) -> Result<Option<String>, CzError> {
        Ok(bridge::event_rows(&self.store, &collection, &name, &phone_zone)?.map(|r| json(&r)))
    }

    /// An edited Android row (`EventRow` JSON: the series, or an exception
    /// with `original_instance`) of a stored object. Only what changed by
    /// meaning becomes a pending edit; returns whether anything did.
    pub fn apply_android_row(
        &self,
        collection: String,
        name: String,
        row: String,
        phone_zone: String,
    ) -> Result<bool, CzError> {
        Ok(bridge::apply_row(&self.store, &collection, &name, &parse_row(&row)?, &phone_zone)?)
    }

    /// A row another app made (`EventRow` JSON): a new object `<uid>.ics`,
    /// waiting for the next sync. Returns its name.
    pub fn create_from_android_row(
        &self,
        collection: String,
        row: String,
        phone_zone: String,
        uid: String,
        fallback_title: String,
    ) -> Result<String, CzError> {
        Ok(bridge::create_from_row(&self.store, &collection, &parse_row(&row)?, &phone_zone, &uid, &fallback_title)?)
    }

    /// A deleted Android row: the whole object, or one occurrence.
    pub fn delete_from_android(
        &self,
        collection: String,
        name: String,
        original_instance: Option<i64>,
        phone_zone: String,
    ) -> Result<(), CzError> {
        Ok(bridge::delete(&self.store, &collection, &name, original_instance, &phone_zone)?)
    }

    /// An event moved to another calendar in another app.
    pub fn move_from_android(
        &self,
        from: String,
        to: String,
        name: String,
        row: String,
        phone_zone: String,
    ) -> Result<(), CzError> {
        Ok(bridge::move_to(&self.store, &from, &to, &name, &parse_row(&row)?, &phone_zone)?)
    }

    // --- editing ---

    /// An edit on the phone: the object's new full text, or `None` to
    /// delete it. It waits for the next sync.
    pub fn edit(&self, collection: String, name: String, body: Option<String>) -> Result<(), CzError> {
        Ok(self.store.edit(&collection, &name, body.as_deref())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Offline;

    impl HttpTransport for Offline {
        fn send(&self, _: String, _: String, _: Option<String>) -> HttpResult {
            HttpResult { status: 0, body: String::new(), error: Some("no network".into()) }
        }
    }

    #[test]
    fn errors_say_what_to_do() {
        let cz = CalZone { store: Store::in_memory().unwrap() };
        assert!(matches!(cz.sync(Arc::new(Offline)), Err(CzError::Network { .. })));
        assert!(matches!(cz.edit("nope".into(), "a.ics".into(), None), Err(CzError::Input { .. })));
        assert_eq!(cz.account().unwrap(), None);
        assert_eq!(cz.collections().unwrap(), "[]");
        let code = format!("czl_{}", "b".repeat(43));
        assert_eq!(
            parse_link(format!("https://cz.example.com/link/{code}")),
            Some(SignInLink { server: "https://cz.example.com".into(), code })
        );
    }
}
