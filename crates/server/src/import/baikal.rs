//! Reading a Baikal account (or any CalDAV/CardDAV server) over the network
//! (decision 19 and 30). Only `PROPFIND` and `REPORT` are ever sent; the
//! [`Method`] type has no other value, so the importer cannot write to the
//! source. Redirects are followed by hand, on the same host only.
//!
//! Discovery: the current user's principal, its calendar and address book
//! homes, the collections in them, then each collection's members, fetched
//! with `calendar-multiget` / `addressbook-multiget` 50 at a time. A batch
//! that does not parse is retried one entry at a time, and an entry that
//! still fails is kept as a failure with the bytes received.

use std::time::Duration;

use calzone_core::import::repair_split_folds;
use calzone_dav::xml::{self, CALDAV, CARDDAV, CS, DAV, Element, ICAL};
use percent_encoding::percent_decode_str;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, LOCATION, WWW_AUTHENTICATE};
use reqwest::{StatusCode, Url};

use super::digest::{self, Challenge};
use super::{Run, Stop};
use crate::domain::{CollectionKind, NewCollection, collections, valid_slug};

const BATCH: usize = 50;
const MAX_REDIRECTS: usize = 3;

/// Where to read from. The password lives only as long as the import.
#[derive(Clone)]
pub struct Source {
    pub url: String,
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source").field("url", &self.url).field("user", &self.user).finish_non_exhaustive()
    }
}

/// The only requests the importer can make: reads.
#[derive(Debug, Clone, Copy)]
enum Method {
    Propfind,
    Report,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Method::Propfind => "PROPFIND",
            Method::Report => "REPORT",
        }
    }
}

/// A checked source URL: http or https, no credentials, no fragment.
pub fn check_url(url: &str) -> Result<Url, String> {
    let u = Url::parse(url.trim()).map_err(|_| "That is not a web address.".to_string())?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err("Use an http:// or https:// address.".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("Put the user name and password in their own fields, not in the address.".into());
    }
    Ok(u)
}

struct Client {
    http: reqwest::Client,
    user: String,
    password: String,
    challenge: Option<Challenge>,
    nc: u32,
}

/// A DAV collection found on the source.
#[derive(Debug, Clone)]
struct Remote {
    url: Url,
    kind: CollectionKind,
    slug: String,
    name: String,
    description: String,
    color: Option<String>,
    components: Vec<String>,
}

fn client() -> reqwest::Client {
    // reqwest is built without a default crypto provider (aws-lc needs cmake
    // everywhere); ring is installed once, before any client exists.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("CalZone/", env!("CARGO_PKG_VERSION"), " (import)"))
        .build()
        .expect("reqwest client")
}

fn unreachable(e: reqwest::Error) -> Stop {
    let what = if e.is_timeout() {
        "it did not answer in time"
    } else if e.is_connect() {
        "could not connect"
    } else {
        "the connection failed"
    };
    Stop::Message(format!("Could not read from the server: {what}."))
}

impl Client {
    fn authorization(&mut self, method: Method, url: &Url) -> Option<String> {
        match self.challenge.as_ref()? {
            Challenge::Basic => Some(digest::basic(&self.user, &self.password)),
            Challenge::Digest { realm, nonce, opaque, qop_auth } => {
                self.nc += 1;
                let mut cnonce = [0u8; 8];
                let _ = getrandom::fill(&mut cnonce);
                let cnonce: String = cnonce.iter().map(|b| format!("{b:02x}")).collect();
                let uri = match url.query() {
                    Some(q) => format!("{}?{q}", url.path()),
                    None => url.path().to_string(),
                };
                Some(digest::digest(
                    &self.user,
                    &self.password,
                    method.name(),
                    &uri,
                    realm,
                    nonce,
                    opaque.as_deref(),
                    *qop_auth,
                    self.nc,
                    &cnonce,
                ))
            }
        }
    }

    /// One request; answers a 401 once with the server's challenge, and
    /// follows same-host redirects. Returns the final URL, status and body.
    async fn send(
        &mut self,
        method: Method,
        url: &Url,
        depth: &str,
        body: &str,
    ) -> Result<(Url, StatusCode, Vec<u8>), Stop> {
        let mut url = url.clone();
        let mut redirects = 0;
        let mut retried_auth = false;
        loop {
            let m = reqwest::Method::from_bytes(method.name().as_bytes()).expect("method");
            let mut req = self
                .http
                .request(m, url.clone())
                .header("Depth", depth)
                .header(CONTENT_TYPE, "application/xml; charset=utf-8")
                .body(body.to_string());
            if let Some(a) = self.authorization(method, &url) {
                req = req.header(AUTHORIZATION, a);
            }
            let resp = req.send().await.map_err(unreachable)?;
            let status = resp.status();
            // First contact, or a stale nonce: answer the challenge once.
            if status == StatusCode::UNAUTHORIZED && !retried_auth {
                let values: Vec<&str> =
                    resp.headers().get_all(WWW_AUTHENTICATE).iter().filter_map(|v| v.to_str().ok()).collect();
                if let Some(c) = digest::pick(values.into_iter()) {
                    self.challenge = Some(c);
                    self.nc = 0;
                    retried_auth = true;
                    continue;
                }
            }
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(Stop::Message("The server refused the user name or password.".into()));
            }
            if status.is_redirection() {
                let next = resp
                    .headers()
                    .get(LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .and_then(|l| url.join(l).ok())
                    .filter(|n| n.host_str() == url.host_str() && matches!(n.scheme(), "http" | "https"));
                match next {
                    Some(n) if redirects < MAX_REDIRECTS => {
                        redirects += 1;
                        url = n;
                        continue;
                    }
                    _ => {
                        return Err(Stop::Message(
                            "The server redirected somewhere else; use its final address.".into(),
                        ));
                    }
                }
            }
            let bytes = resp.bytes().await.map_err(unreachable)?.to_vec();
            return Ok((url, status, bytes));
        }
    }

    /// A PROPFIND that must answer 207; the parsed multistatus.
    async fn propfind(&mut self, url: &Url, depth: &str, props: &str) -> Result<(Url, Element), Stop> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:" xmlns:c="{CALDAV}" xmlns:card="{CARDDAV}" xmlns:cs="{CS}" xmlns:ical="{ICAL}"><d:prop>{props}</d:prop></d:propfind>"#
        );
        let (final_url, status, bytes) = self.send(Method::Propfind, url, depth, &body).await?;
        if status != StatusCode::MULTI_STATUS {
            return Err(Stop::Message(format!(
                "{} did not answer as a CalDAV/CardDAV server (HTTP {}).",
                final_url.path(),
                status.as_u16()
            )));
        }
        let root = parse(&bytes).ok_or_else(|| Stop::Message("The server's answer did not parse.".into()))?;
        Ok((final_url, root))
    }
}

fn parse(bytes: &[u8]) -> Option<Element> {
    let (fixed, _) = repair_split_folds(bytes);
    xml::parse(&fixed).ok()
}

/// The `<prop>` of a response's 200 propstat.
fn ok_prop(resp: &Element) -> Option<&Element> {
    resp.children_named(DAV, "propstat")
        .find(|ps| ps.child(DAV, "status").is_some_and(|s| s.text.contains(" 200")))
        .and_then(|ps| ps.child(DAV, "prop"))
}

fn href_in(prop: &Element, ns: &str, local: &str) -> Option<String> {
    prop.child(ns, local)?.child(DAV, "href").map(|h| h.trimmed_text().to_string())
}

/// The last path segment, percent-decoded.
fn last_segment(url: &Url) -> String {
    let seg = url.path().trim_end_matches('/').rsplit('/').next().unwrap_or("");
    percent_decode_str(seg).decode_utf8_lossy().into_owned()
}

/// Same URL ignoring a trailing slash.
fn same(a: &Url, b: &Url) -> bool {
    a.as_str().trim_end_matches('/') == b.as_str().trim_end_matches('/')
}

fn color(raw: &str) -> Option<String> {
    let hex = raw.trim().strip_prefix('#')?;
    let hex = hex.get(..6)?;
    hex.bytes().all(|b| b.is_ascii_hexdigit()).then(|| format!("#{}", hex.to_ascii_lowercase()))
}

impl Run {
    /// Import every calendar and address book of a Baikal account, then
    /// close the report; returns its id. Each remote collection maps to the
    /// account's own collection with the same slug and kind, made when
    /// missing, so a second run lands in the same place and changes nothing.
    pub async fn baikal(mut self, source: Source) -> uuid::Uuid {
        let outcome = self.baikal_inner(source).await;
        self.finish(outcome).await
    }

    async fn baikal_inner(&mut self, source: Source) -> Result<(), Stop> {
        let base = check_url(&source.url).map_err(Stop::Message)?;
        let mut c = Client { http: client(), user: source.user, password: source.password, challenge: None, nc: 0 };
        let remotes = discover(&mut c, &base).await?;
        if remotes.is_empty() {
            return Err(Stop::Message("No calendars or address books were found for this account.".into()));
        }
        for r in remotes {
            self.remote(&mut c, r).await?;
        }
        Ok(())
    }

    async fn remote(&mut self, c: &mut Client, r: Remote) -> Result<(), Stop> {
        let slug = if valid_slug(&r.slug) { r.slug.clone() } else { collections::slugify(&r.name, "imported") };
        let comps: Vec<&str> = r.components.iter().map(String::as_str).collect();
        let new = NewCollection {
            kind: r.kind,
            slug: "",
            display_name: if r.name.is_empty() { &slug } else { &r.name },
            description: &r.description,
            color: r.color.as_deref(),
            components: &comps,
        };
        let (local, created) = self.collection_named(r.kind, &slug, new).await?;
        let slot = self.slot(&local, created);
        let (url, listing) = c.propfind(&r.url, "1", "<d:resourcetype/><d:getetag/>").await?;
        let mut members = Vec::new();
        for resp in listing.children_named(DAV, "response") {
            let Some(href) = resp.child(DAV, "href").map(|h| h.trimmed_text().to_string()) else { continue };
            let Ok(u) = url.join(&href) else { continue };
            let is_collection = ok_prop(resp)
                .and_then(|p| p.child(DAV, "resourcetype"))
                .is_some_and(|t| t.child(DAV, "collection").is_some());
            if same(&u, &url) || is_collection {
                continue;
            }
            members.push((href, u));
        }
        let (report, data) = match r.kind {
            CollectionKind::Calendar => ("c:calendar-multiget", "<c:calendar-data/>"),
            CollectionKind::AddressBook => ("card:addressbook-multiget", "<card:address-data/>"),
        };
        let ext = if r.kind == CollectionKind::Calendar { "ics" } else { "vcf" };
        for chunk in members.chunks(BATCH) {
            let got = multiget(c, &url, report, data, chunk).await?;
            for (href, u) in chunk {
                let name = last_segment(u);
                match got.iter().find(|(h, _)| h == href || url.join(h).is_ok_and(|x| same(&x, u))) {
                    Some((_, Ok(body))) => {
                        let name = if crate::domain::valid_resource_name(&name) {
                            name
                        } else {
                            super::resource_name(body, ext)
                        };
                        self.object(slot, local.id, &name, body).await?;
                    }
                    Some((_, Err(raw))) => {
                        self.fail(slot, "the server sent this entry in a form that could not be read".into(), raw)
                            .await?;
                    }
                    None => {
                        self.fail(slot, "the server listed this entry but did not send it".into(), href.as_bytes())
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// One multiget; per href the body, or the raw bytes when it could not be
/// read. A batch that does not parse is asked again one entry at a time.
async fn multiget(
    c: &mut Client,
    url: &Url,
    report: &str,
    data: &str,
    chunk: &[(String, Url)],
) -> Result<Vec<(String, Result<String, Vec<u8>>)>, Stop> {
    let ask = |hrefs: &[(String, Url)]| {
        let list: String = hrefs.iter().map(|(h, _)| format!("<d:href>{}</d:href>", xml::escape(h))).collect();
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><{report} xmlns:d="DAV:" xmlns:c="{CALDAV}" xmlns:card="{CARDDAV}"><d:prop><d:getetag/>{data}</d:prop>{list}</{report}>"#
        )
    };
    let (_, status, bytes) = c.send(Method::Report, url, "1", &ask(chunk)).await?;
    if status != StatusCode::MULTI_STATUS {
        return Err(Stop::Message(format!("The server refused to send entries (HTTP {}).", status.as_u16())));
    }
    if let Some(root) = parse(&bytes) {
        return Ok(bodies(&root));
    }
    let mut out = Vec::new();
    for one in chunk {
        let (_, _, bytes) = c.send(Method::Report, url, "1", &ask(std::slice::from_ref(one))).await?;
        match parse(&bytes) {
            Some(root) => out.extend(bodies(&root)),
            None => out.push((one.0.clone(), Err(bytes))),
        }
    }
    Ok(out)
}

fn bodies(root: &Element) -> Vec<(String, Result<String, Vec<u8>>)> {
    let mut out = Vec::new();
    for resp in root.children_named(DAV, "response") {
        let Some(href) = resp.child(DAV, "href").map(|h| h.trimmed_text().to_string()) else { continue };
        let data = ok_prop(resp)
            .and_then(|p| p.child(CALDAV, "calendar-data").or_else(|| p.child(CARDDAV, "address-data")))
            .map(|d| d.text.clone())
            .filter(|t| !t.trim().is_empty());
        if let Some(body) = data {
            out.push((href, Ok(body)));
        }
    }
    out
}

/// Every calendar and address book of the signed-in user.
async fn discover(c: &mut Client, base: &Url) -> Result<Vec<Remote>, Stop> {
    let homes_props = "<d:current-user-principal/><c:calendar-home-set/><card:addressbook-home-set/>";
    let (url, root) = c.propfind(base, "0", homes_props).await?;
    let first = root.children_named(DAV, "response").find_map(ok_prop);
    let principal = first.and_then(|p| href_in(p, DAV, "current-user-principal")).and_then(|h| url.join(&h).ok());
    let mut homes: Vec<Url> = Vec::new();
    let mut add_homes = |p: &Element, from: &Url| {
        for (ns, local) in [(CALDAV, "calendar-home-set"), (CARDDAV, "addressbook-home-set")] {
            for h in p.child(ns, local).into_iter().flat_map(|e| e.children_named(DAV, "href")) {
                if let Ok(u) = from.join(h.trimmed_text())
                    && !homes.iter().any(|x| same(x, &u))
                {
                    homes.push(u);
                }
            }
        }
    };
    if let Some(p) = first {
        add_homes(p, &url);
    }
    if let Some(pr) = principal.filter(|p| !same(p, &url)) {
        let (purl, proot) = c.propfind(&pr, "0", homes_props).await?;
        if let Some(p) = proot.children_named(DAV, "response").find_map(ok_prop) {
            add_homes(p, &purl);
        }
    }
    if homes.is_empty() {
        // The address may already be a home (or a collection's parent).
        homes.push(url.clone());
    }
    let props = "<d:resourcetype/><d:displayname/><ical:calendar-color/><c:supported-calendar-component-set/>\
                 <c:calendar-description/><card:addressbook-description/>";
    let mut out: Vec<Remote> = Vec::new();
    for home in homes {
        let (hurl, root) = c.propfind(&home, "1", props).await?;
        for resp in root.children_named(DAV, "response") {
            let Some(p) = ok_prop(resp) else { continue };
            let Some(href) = resp.child(DAV, "href") else { continue };
            let Ok(u) = hurl.join(href.trimmed_text()) else { continue };
            let Some(t) = p.child(DAV, "resourcetype") else { continue };
            let kind = if t.child(CALDAV, "calendar").is_some() {
                CollectionKind::Calendar
            } else if t.child(CARDDAV, "addressbook").is_some() {
                CollectionKind::AddressBook
            } else {
                continue;
            };
            if out.iter().any(|r| same(&r.url, &u)) {
                continue;
            }
            let text =
                |ns: &str, local: &str| p.child(ns, local).map(|e| e.trimmed_text().to_string()).unwrap_or_default();
            let components = p
                .child(CALDAV, "supported-calendar-component-set")
                .map(|s| {
                    s.children_named(CALDAV, "comp")
                        .filter_map(|c| c.attr("name").map(str::to_ascii_uppercase))
                        .filter(|n| matches!(n.as_str(), "VEVENT" | "VTODO" | "VJOURNAL"))
                        .collect()
                })
                .unwrap_or_default();
            let mut dir = u.clone();
            if !dir.path().ends_with('/') {
                dir.set_path(&format!("{}/", dir.path()));
            }
            out.push(Remote {
                slug: last_segment(&u),
                url: dir,
                kind,
                name: text(DAV, "displayname").chars().take(100).collect(),
                description: if kind == CollectionKind::Calendar {
                    text(CALDAV, "calendar-description")
                } else {
                    text(CARDDAV, "addressbook-description")
                }
                .chars()
                .take(1000)
                .collect(),
                color: color(&text(ICAL, "calendar-color")).filter(|_| kind == CollectionKind::Calendar),
                components,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_bits() {
        assert!(check_url("https://dav.example.org/dav.php/").is_ok());
        assert!(check_url("ftp://x/").is_err());
        assert!(check_url("https://me:pw@x/").is_err());
        assert!(check_url("not a url").is_err());
        let u = Url::parse("https://x/dav.php/calendars/anna/fam%20ily/").unwrap();
        assert_eq!(last_segment(&u), "fam ily");
        assert_eq!(color("#FF8800FF").as_deref(), Some("#ff8800"));
        assert_eq!(color("red"), None);
        assert!(same(&Url::parse("https://x/a/").unwrap(), &Url::parse("https://x/a").unwrap()));
    }
}
