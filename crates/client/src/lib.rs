//! calzone-client: the phone's side of sync (decisions 14, 15 and 48).
//!
//! - [`Store`]: SQLite on the phone. Every object's full text (the truth,
//!   lossless like the server's) plus a projection for queries, the
//!   person's pending edits with the copy each started from, the conflicts
//!   to show, and a local change log the system bridge reads to mirror
//!   what came from the server into Android's calendar and contacts.
//! - [`engine`]: one sync run. List the collections, upload pending edits
//!   (the server merges them, decision 46), pull what changed. A run that
//!   fails part way keeps the last good copy and every pending edit.
//! - [`Transport`]: the HTTP itself is the app's (OkHttp), so TLS, the
//!   phone's certificates and GrapheneOS's Network permission all stay
//!   Android's. The engine only says what to send.
//! - [`bridge`]: Android's calendar: rows for a stored event, Android's
//!   edits as pending edits (decision 49).
//!
//! Talks to the server only through the sync API (`/sync/v1`).

pub mod bridge;
pub mod engine;
pub mod link;
pub mod store;
pub mod transport;

pub use engine::Report;
pub use store::{Conflict, ConflictDetail, LocalChange, ObjectMeta, Store, StoredCollection, StoredObject};
pub use transport::{Method, Request, Response, Transport};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server could not be reached, or the answer did not arrive. Try
    /// again later; nothing was lost.
    #[error("network: {0}")]
    Network(String),
    /// The server no longer knows this phone (removed on the Devices page,
    /// or its person disabled): sign in again.
    #[error("signed out: this phone's token is not valid any more")]
    SignedOut,
    /// An answer the engine did not expect. `code` is the server's error
    /// code when it sent one.
    #[error("server answered {status}: {message}")]
    Server { status: u16, code: String, message: String },
    /// The app asked for something that cannot be done (an unknown
    /// collection, an object that does not parse).
    #[error("{0}")]
    Input(String),
    #[error("store: {0}")]
    Store(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
