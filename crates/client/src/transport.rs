//! What the engine asks the app's HTTP client to send. The transport adds
//! the server's address in front of `path` and, once signed in, the
//! `Authorization: Bearer` header; the engine never sees either.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    /// From the server's root: `/sync/v1/...`, with any query string.
    pub path: String,
    /// JSON, sent as `application/json`.
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// The app's HTTP client. `Err` is "no answer" (no network, a timeout, a
/// TLS failure): the engine stops and keeps everything for the next run.
/// Any answer, whatever its status, is `Ok`.
pub trait Transport {
    fn send(&self, request: &Request) -> Result<Response, String>;
}
