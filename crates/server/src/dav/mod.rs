//! The CalDAV/CardDAV door (decisions 3 and 28): auth, then the protocol
//! crate parses and the domain service does the work. Mounted only when
//! `CALZONE_DAV` is on.

pub mod auth;
mod props;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use calzone_dav::path::{self, Kind, Target};
use calzone_dav::request::{self, MakeCollection, PatchOp, PropRequest, Report, RequestError};
use calzone_dav::response::{Multistatus, Response as DavResponse, error_body};
use calzone_dav::xml::{CALDAV, CARDDAV, DAV, Element, ICAL, QName};
use calzone_dav::{ALLOW, DAV_HEADER, MAX_RESOURCE_SIZE, MAX_XML_SIZE, parse_sync_token, sync_token};

use crate::AppState;
use crate::client::ClientAddr;
use crate::domain::app_passwords::Login;
use crate::domain::objects::{self, ObjectMeta, Precondition, Served};
use crate::domain::{Collection, CollectionKind, DomainError, NewCollection, collections};
use props::{Resource, collection_href, object_href};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/.well-known/caldav", any(well_known))
        .route("/.well-known/carddav", any(well_known))
        .route("/dav", any(handle))
        .route("/dav/", any(handle))
        .route("/dav/{*rest}", any(handle))
}

/// RFC 6764: the context path. Every method is redirected, PROPFIND too.
async fn well_known() -> Response {
    (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, "/dav/")]).into_response()
}

/// Why a request did not succeed.
enum Fail {
    Status(StatusCode),
    /// A status with a `DAV:error` precondition element.
    Condition(StatusCode, String),
    Domain(DomainError),
}

impl From<DomainError> for Fail {
    fn from(e: DomainError) -> Self {
        Fail::Domain(e)
    }
}

impl From<RequestError> for Fail {
    fn from(e: RequestError) -> Self {
        match e {
            RequestError::Filter(calzone_dav::filter::FilterError::Unsupported(_)) => {
                Fail::Condition(StatusCode::FORBIDDEN, "<c:supported-filter/>".into())
            }
            _ => Fail::Status(StatusCode::BAD_REQUEST),
        }
    }
}

fn xml_response(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        match self {
            Fail::Status(s) => s.into_response(),
            Fail::Condition(s, c) => xml_response(s, error_body(&c)),
            Fail::Domain(e) => match e {
                DomainError::NotFound => StatusCode::NOT_FOUND.into_response(),
                DomainError::ReadOnly => xml_response(StatusCode::FORBIDDEN, error_body("<d:need-privileges/>")),
                DomainError::PreconditionFailed => StatusCode::PRECONDITION_FAILED.into_response(),
                DomainError::UidConflict { .. } => {
                    xml_response(StatusCode::FORBIDDEN, error_body("<c:no-uid-conflict/>"))
                }
                DomainError::Invalid(_) => xml_response(StatusCode::FORBIDDEN, error_body("<c:valid-calendar-data/>")),
                DomainError::WrongKind => {
                    xml_response(StatusCode::FORBIDDEN, error_body("<c:supported-calendar-data/>"))
                }
                DomainError::ComponentNotAccepted => {
                    xml_response(StatusCode::FORBIDDEN, error_body("<c:supported-calendar-component/>"))
                }
                DomainError::BadName => StatusCode::FORBIDDEN.into_response(),
                DomainError::NameTaken => StatusCode::METHOD_NOT_ALLOWED.into_response(),
                DomainError::BadSyncToken => xml_response(StatusCode::FORBIDDEN, error_body("<d:valid-sync-token/>")),
                DomainError::Db(e) => {
                    tracing::error!(error = %e, "dav: database error");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            },
        }
    }
}

type Out = Result<Response, Fail>;

/// One authenticated request.
struct Ctx<'a> {
    st: &'a AppState,
    login: &'a Login,
}

impl Ctx<'_> {
    fn me(&self) -> &str {
        &self.login.principal.name
    }

    fn actor(&self) -> uuid::Uuid {
        self.login.principal.id
    }

    /// A collection by path, its access capped by the password's scope.
    /// Outside the scope, or of the other kind, it is `NotFound`.
    async fn collection(&self, kind: Kind, owner: &str, slug: &str) -> Result<Collection, Fail> {
        let c = collections::by_path(&self.st.pool, self.actor(), owner, slug).await?;
        if props::kind_of(&c) != kind {
            return Err(DomainError::NotFound.into());
        }
        let access = self.login.cap(c.id, c.access).ok_or(DomainError::NotFound)?;
        Ok(Collection { access, ..c })
    }

    async fn visible(&self, kind: Kind) -> Result<Vec<Collection>, Fail> {
        let k = match kind {
            Kind::Calendar => CollectionKind::Calendar,
            Kind::AddressBook => CollectionKind::AddressBook,
        };
        let all = collections::visible(&self.st.pool, self.actor(), k).await?;
        Ok(all
            .into_iter()
            .filter_map(|c| {
                let access = self.login.cap(c.id, c.access)?;
                Some(Collection { access, ..c })
            })
            .collect())
    }

    /// Homes and principals exist only for the logged-in user.
    fn own(&self, user: &str) -> Result<(), Fail> {
        if user == self.me() { Ok(()) } else { Err(DomainError::NotFound.into()) }
    }
}

/// Every DAV request. Also the fallback of `/` (see `web::routes`): some
/// clients PROPFIND the bare server address (python caldav, Thunderbird with
/// a host only); GET there is the web app's, everything else 404.
pub async fn handle(State(st): State<AppState>, addr: ClientAddr, req: Request) -> Response {
    let method = req.method().as_str().to_ascii_uppercase();
    let at_root = req.uri().path() == "/";
    if at_root && method != "PROPFIND" {
        return StatusCode::NOT_FOUND.into_response();
    }
    if method == "OPTIONS" {
        return options();
    }
    let login = match auth::authenticate(&st, &addr, req.headers()).await {
        Ok(l) => l,
        Err(resp) => return resp,
    };
    let target = if at_root { Some(Target::Root) } else { path::parse_path(req.uri().path()) };
    let Some(target) = target else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let root_href = if at_root { "/".to_string() } else { path::href(&Target::Root) };
    let headers = req.headers().clone();
    let limit = if method == "PUT" { MAX_RESOURCE_SIZE } else { MAX_XML_SIZE };
    let Ok(body) = axum::body::to_bytes(req.into_body(), limit).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let ctx = Ctx { st: &st, login: &login };
    let out = match method.as_str() {
        "PROPFIND" => propfind(&ctx, &target, &root_href, &headers, &body).await,
        "PROPPATCH" => proppatch(&ctx, &target, &body).await,
        "REPORT" => report(&ctx, &target, &body).await,
        "GET" | "HEAD" => get(&ctx, &target).await,
        "PUT" => put(&ctx, &target, &headers, &body).await,
        "DELETE" => delete(&ctx, &target, &headers).await,
        "MKCALENDAR" => mkcol(&ctx, &target, &body, true).await,
        "MKCOL" => mkcol(&ctx, &target, &body, false).await,
        _ => Err(Fail::Status(StatusCode::METHOD_NOT_ALLOWED)),
    };
    out.unwrap_or_else(IntoResponse::into_response)
}

fn options() -> Response {
    let mut h = HeaderMap::new();
    h.insert("dav", HeaderValue::from_static(DAV_HEADER));
    h.insert(header::ALLOW, HeaderValue::from_static(ALLOW));
    (StatusCode::OK, h).into_response()
}

fn multistatus(ms: Multistatus) -> Out {
    Ok(xml_response(StatusCode::MULTI_STATUS, ms.render()))
}

// ---------------------------------------------------------------- PROPFIND

/// `root_href`: the root answers under the path it was asked at (`/dav/` or `/`).
async fn propfind(ctx: &Ctx<'_>, target: &Target, root_href: &str, headers: &HeaderMap, body: &[u8]) -> Out {
    // Clients always send Depth; a missing one is read as 0, and infinity
    // is refused (RFC 4918 9.1).
    let depth1 = match headers.get("depth").and_then(|v| v.to_str().ok()).map(str::trim) {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => return Err(Fail::Condition(StatusCode::FORBIDDEN, "<d:propfind-finite-depth/>".into())),
    };
    let req = request::parse_propfind(body)?;
    let me = ctx.me().to_string();
    let mut ms = Multistatus::default();
    match target {
        Target::Root => ms.responses.push(props::response(root_href.to_string(), &Resource::Root, &me, &req)),
        Target::Principal { user } => {
            ctx.own(user)?;
            let p = &ctx.login.principal;
            let res = Resource::Principal { name: &p.name, display_name: &p.display_name };
            ms.responses.push(props::response(path::href(target), &res, &me, &req));
        }
        Target::Home { kind, user } => {
            ctx.own(user)?;
            ms.responses.push(props::response(path::href(target), &Resource::Home(*kind), &me, &req));
            if depth1 {
                for c in ctx.visible(*kind).await? {
                    ms.responses.push(props::response(collection_href(&c), &Resource::Collection(&c), &me, &req));
                }
            }
        }
        Target::Collection { kind, owner, slug } => {
            let c = ctx.collection(*kind, owner, slug).await?;
            ms.responses.push(props::response(collection_href(&c), &Resource::Collection(&c), &me, &req));
            if depth1 {
                if props::wants_data(&req) {
                    let names: Vec<String> =
                        objects::list(&ctx.st.pool, ctx.actor(), c.id).await?.into_iter().map(|m| m.name).collect();
                    for s in objects::get_many(&ctx.st.pool, ctx.actor(), c.id, &names).await? {
                        ms.responses.push(object_response(&c, &s.meta, Some(&s.body), &me, &req));
                    }
                } else {
                    for m in objects::list(&ctx.st.pool, ctx.actor(), c.id).await? {
                        ms.responses.push(object_response(&c, &m, None, &me, &req));
                    }
                }
            }
        }
        Target::Object { kind, owner, slug, name } => {
            let c = ctx.collection(*kind, owner, slug).await?;
            let s = objects::get(&ctx.st.pool, ctx.actor(), c.id, name).await?;
            ms.responses.push(object_response(&c, &s.meta, Some(&s.body), &me, &req));
        }
    }
    multistatus(ms)
}

fn object_response(c: &Collection, meta: &ObjectMeta, body: Option<&str>, me: &str, req: &PropRequest) -> DavResponse {
    props::response(object_href(c, &meta.name), &Resource::Object { meta, body }, me, req)
}

// --------------------------------------------------------------- PROPPATCH

/// What a PROPPATCH or MKCALENDAR may set on a collection.
#[derive(Default)]
struct Settable {
    display_name: Option<String>,
    description: Option<String>,
    color: Option<Option<String>>,
    components: Option<Vec<String>>,
}

fn valid_color(c: &str) -> bool {
    let hex = c.strip_prefix('#').unwrap_or("");
    matches!(hex.len(), 3 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Apply one property to `s`; false when it cannot be set on this kind.
fn settable(s: &mut Settable, kind: Kind, el: Option<&Element>, name: &QName, creating: bool) -> bool {
    let text = || el.map(|e| e.trimmed_text().to_string()).unwrap_or_default();
    match (name.ns.as_str(), name.local.as_str(), kind) {
        (DAV, "displayname", _) => s.display_name = Some(text()),
        (CALDAV, "calendar-description", Kind::Calendar) | (CARDDAV, "addressbook-description", Kind::AddressBook) => {
            s.description = Some(text())
        }
        (ICAL, "calendar-color", Kind::Calendar) => {
            let t = text();
            if el.is_none() || t.is_empty() {
                s.color = Some(None);
            } else if valid_color(&t) {
                s.color = Some(Some(t));
            } else {
                return false;
            }
        }
        (CALDAV, "supported-calendar-component-set", Kind::Calendar) if creating => {
            let comps: Vec<String> = el
                .map(|e| {
                    e.children_named(CALDAV, "comp")
                        .filter_map(|c| c.attr("name"))
                        .map(str::to_ascii_uppercase)
                        .collect()
                })
                .unwrap_or_default();
            s.components = Some(comps);
        }
        // DAVx5 sends these when it creates a calendar. Times are evaluated
        // with each object's own zones, so they are accepted and not kept.
        (CALDAV, "calendar-timezone" | "calendar-timezone-id", Kind::Calendar) if creating => {}
        (ICAL, "calendar-order", Kind::Calendar) if creating => {}
        _ => return false,
    }
    true
}

async fn proppatch(ctx: &Ctx<'_>, target: &Target, body: &[u8]) -> Out {
    let ops = request::parse_proppatch(body)?;
    let Target::Collection { kind, owner, slug } = target else {
        // Homes, principals and objects have no writable properties.
        let names: Vec<QName> = ops.iter().map(op_name).collect();
        return multistatus(Multistatus {
            responses: vec![DavResponse::Props {
                href: path::href(target),
                ok: vec![],
                missing: vec![],
                other: vec![(403, names)],
            }],
            sync_token: None,
        });
    };
    let c = ctx.collection(*kind, owner, slug).await?;
    if !c.access.can_admin() {
        return Err(DomainError::ReadOnly.into());
    }
    let mut s = Settable::default();
    let mut refused = Vec::new();
    for op in &ops {
        let name = op_name(op);
        let el = match op {
            PatchOp::Set(e) => Some(e),
            PatchOp::Remove(_) => None,
        };
        if !settable(&mut s, *kind, el, &name, false) {
            refused.push(name);
        }
    }
    let names: Vec<QName> = ops.iter().map(op_name).collect();
    let response = if refused.is_empty() {
        collections::update_properties(
            &ctx.st.pool,
            ctx.actor(),
            c.id,
            s.display_name.as_deref(),
            s.description.as_deref(),
            s.color.as_ref().map(|c| c.as_deref()),
        )
        .await?;
        let ok = names.into_iter().map(|n| calzone_dav::response::Prop::new(n, calzone_dav::response::Value::Empty));
        DavResponse::Props { href: collection_href(&c), ok: ok.collect(), missing: vec![], other: vec![] }
    } else {
        // All or nothing (RFC 4918 9.2): the rest fail by dependency.
        let rest: Vec<QName> = names.into_iter().filter(|n| !refused.contains(n)).collect();
        let mut other = vec![(403, refused)];
        if !rest.is_empty() {
            other.push((424, rest));
        }
        DavResponse::Props { href: collection_href(&c), ok: vec![], missing: vec![], other }
    };
    multistatus(Multistatus { responses: vec![response], sync_token: None })
}

fn op_name(op: &PatchOp) -> QName {
    match op {
        PatchOp::Set(e) => e.name.clone(),
        PatchOp::Remove(n) => n.clone(),
    }
}

// ------------------------------------------------------- MKCALENDAR / MKCOL

async fn mkcol(ctx: &Ctx<'_>, target: &Target, body: &[u8], mkcalendar: bool) -> Out {
    let Target::Collection { kind, owner, slug } = target else {
        return Err(Fail::Status(StatusCode::FORBIDDEN));
    };
    // Only in your own home, and only with a password that is not scoped:
    // a scoped password cannot reach what it would create.
    if owner != ctx.me() || ctx.login.scope.is_some() {
        return Err(Fail::Status(StatusCode::FORBIDDEN));
    }
    let mk: MakeCollection = request::parse_mkcol(body)?;
    let want = if mkcalendar {
        if *kind != Kind::Calendar {
            return Err(Fail::Status(StatusCode::FORBIDDEN));
        }
        Kind::Calendar
    } else {
        let rt = mk.resourcetype.clone().unwrap_or_default();
        let is = |ns: &str, l: &str| rt.iter().any(|q| q.is(ns, l));
        match (is(CALDAV, "calendar"), is(CARDDAV, "addressbook")) {
            (true, false) => Kind::Calendar,
            (false, true) => Kind::AddressBook,
            // Plain WebDAV collections have no place here.
            _ => return Err(Fail::Condition(StatusCode::FORBIDDEN, "<d:valid-resourcetype/>".into())),
        }
    };
    if want != *kind {
        return Err(Fail::Condition(StatusCode::FORBIDDEN, "<d:valid-resourcetype/>".into()));
    }
    let mut s = Settable::default();
    let mut refused = Vec::new();
    for p in &mk.props {
        if !settable(&mut s, want, Some(p), &p.name, true) {
            refused.push(p.name.clone());
        }
    }
    if !refused.is_empty() {
        let rest: Vec<QName> = mk.props.iter().map(|p| p.name.clone()).filter(|n| !refused.contains(n)).collect();
        let mut other = vec![(403, refused)];
        if !rest.is_empty() {
            other.push((424, rest));
        }
        let href = path::href(target);
        let ms = Multistatus {
            responses: vec![DavResponse::Props { href, ok: vec![], missing: vec![], other }],
            sync_token: None,
        };
        return Ok(xml_response(StatusCode::FORBIDDEN, ms.render()));
    }
    let components: Vec<String> = s.components.unwrap_or_default();
    let comp_refs: Vec<&str> = components.iter().map(String::as_str).collect();
    let color = s.color.flatten();
    collections::create(
        &ctx.st.pool,
        ctx.actor(),
        NewCollection {
            kind: match want {
                Kind::Calendar => CollectionKind::Calendar,
                Kind::AddressBook => CollectionKind::AddressBook,
            },
            slug,
            display_name: s.display_name.as_deref().unwrap_or(""),
            description: s.description.as_deref().unwrap_or(""),
            color: color.as_deref(),
            components: &comp_refs,
        },
    )
    .await?;
    Ok(StatusCode::CREATED.into_response())
}

// ------------------------------------------------------------ GET/PUT/DELETE

fn etag_header(etag: &str) -> [(header::HeaderName, String); 1] {
    [(header::ETAG, etag.to_string())]
}

async fn get(ctx: &Ctx<'_>, target: &Target) -> Out {
    let Target::Object { kind, owner, slug, name } = target else {
        return Err(Fail::Status(StatusCode::METHOD_NOT_ALLOWED));
    };
    let c = ctx.collection(*kind, owner, slug).await?;
    let s: Served = objects::get(&ctx.st.pool, ctx.actor(), c.id, name).await?;
    let mut resp = (StatusCode::OK, etag_header(&s.meta.etag()), Body::from(s.body)).into_response();
    if let Ok(v) = HeaderValue::from_str(&props::content_type(&s.meta)) {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    Ok(resp)
}

fn precondition(headers: &HeaderMap) -> Precondition {
    let h = |n: header::HeaderName| headers.get(n).and_then(|v| v.to_str().ok()).map(|v| v.trim().to_string());
    if let Some(m) = h(header::IF_MATCH) {
        return if m == "*" { Precondition::IfMatchAny } else { Precondition::IfMatch(m) };
    }
    match h(header::IF_NONE_MATCH) {
        Some(m) if m == "*" => Precondition::IfNoneMatchAny,
        _ => Precondition::None,
    }
}

async fn put(ctx: &Ctx<'_>, target: &Target, headers: &HeaderMap, body: &[u8]) -> Out {
    let Target::Object { kind, owner, slug, name } = target else {
        return Err(Fail::Status(StatusCode::METHOD_NOT_ALLOWED));
    };
    let c = ctx.collection(*kind, owner, slug).await?;
    if !c.access.can_write() {
        return Err(DomainError::ReadOnly.into());
    }
    let calendar = *kind == Kind::Calendar;
    let invalid = || {
        Fail::Condition(
            StatusCode::FORBIDDEN,
            if calendar { "<c:valid-calendar-data/>" } else { "<card:valid-address-data/>" }.into(),
        )
    };
    let Ok(text) = std::str::from_utf8(body) else { return Err(invalid()) };
    match objects::put(&ctx.st.pool, ctx.actor(), c.id, name, text, precondition(headers)).await {
        Ok(w) => {
            let status = if w.created { StatusCode::CREATED } else { StatusCode::NO_CONTENT };
            Ok((status, etag_header(&w.meta.etag())).into_response())
        }
        Err(DomainError::Invalid(_)) => Err(invalid()),
        Err(DomainError::WrongKind) => Err(Fail::Condition(
            StatusCode::FORBIDDEN,
            if calendar { "<c:supported-calendar-data/>" } else { "<card:supported-address-data/>" }.into(),
        )),
        Err(DomainError::UidConflict { existing }) => {
            let el = if calendar { "c:no-uid-conflict" } else { "card:no-uid-conflict" };
            let href = calzone_dav::xml::escape(&object_href(&c, &existing));
            Err(Fail::Condition(StatusCode::FORBIDDEN, format!("<{el}><d:href>{href}</d:href></{el}>")))
        }
        Err(e) => Err(e.into()),
    }
}

async fn delete(ctx: &Ctx<'_>, target: &Target, headers: &HeaderMap) -> Out {
    match target {
        Target::Object { kind, owner, slug, name } => {
            let c = ctx.collection(*kind, owner, slug).await?;
            if !c.access.can_write() {
                // Still 404 for a missing object, so the order matches the domain.
                objects::get(&ctx.st.pool, ctx.actor(), c.id, name).await?;
                return Err(DomainError::ReadOnly.into());
            }
            objects::delete(&ctx.st.pool, ctx.actor(), c.id, name, precondition(headers)).await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Target::Collection { kind, owner, slug } => {
            let c = ctx.collection(*kind, owner, slug).await?;
            if !c.access.can_admin() {
                return Err(DomainError::ReadOnly.into());
            }
            collections::delete(&ctx.st.pool, ctx.actor(), c.id).await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        _ => Err(Fail::Status(StatusCode::FORBIDDEN)),
    }
}

// ------------------------------------------------------------------ REPORT

async fn report(ctx: &Ctx<'_>, target: &Target, body: &[u8]) -> Out {
    let report = request::parse_report(body)?;
    let unsupported = || Fail::Condition(StatusCode::FORBIDDEN, "<d:supported-report/>".into());
    let Target::Collection { kind, owner, slug } = target else { return Err(unsupported()) };
    let c = ctx.collection(*kind, owner, slug).await?;
    let me = ctx.me().to_string();
    let pool = &ctx.st.pool;
    let calendar = *kind == Kind::Calendar;
    match report {
        Report::SyncCollection { token, props: req, .. } => {
            let since = if token.is_empty() {
                None
            } else {
                Some(parse_sync_token(&c.id.to_string(), &token).ok_or(DomainError::BadSyncToken)?)
            };
            let changes = objects::changes_since(pool, ctx.actor(), c.id, since).await?;
            let mut ms =
                Multistatus { responses: Vec::new(), sync_token: Some(sync_token(&c.id.to_string(), changes.token)) };
            if props::wants_data(&req) {
                let names: Vec<String> = changes.changed.iter().map(|m| m.name.clone()).collect();
                for s in objects::get_many(pool, ctx.actor(), c.id, &names).await? {
                    ms.responses.push(object_response(&c, &s.meta, Some(&s.body), &me, &req));
                }
            } else {
                for m in &changes.changed {
                    ms.responses.push(object_response(&c, m, None, &me, &req));
                }
            }
            for name in &changes.deleted {
                ms.responses.push(DavResponse::Status { href: object_href(&c, name), status: 404 });
            }
            multistatus(ms)
        }
        Report::CalendarMultiget { props: req, hrefs } if calendar => multiget(ctx, &c, &req, &hrefs).await,
        Report::AddressbookMultiget { props: req, hrefs } if !calendar => multiget(ctx, &c, &req, &hrefs).await,
        Report::CalendarQuery { props: req, filter } if calendar => {
            let candidates = match &filter.time_range {
                Some(r) => {
                    // Open ends as dates Postgres can store.
                    let from = r.start.unwrap_or_else(|| far(1000));
                    let to = r.end.unwrap_or_else(|| far(9999));
                    objects::in_range(pool, ctx.actor(), c.id, from, to).await?
                }
                None => objects::list(pool, ctx.actor(), c.id).await?,
            };
            let names: Vec<String> = candidates.into_iter().map(|m| m.name).collect();
            let mut ms = Multistatus::default();
            for s in objects::get_many(pool, ctx.actor(), c.id, &names).await? {
                if filter.matches(&s.body) {
                    ms.responses.push(object_response(&c, &s.meta, Some(&s.body), &me, &req));
                }
            }
            multistatus(ms)
        }
        Report::AddressbookQuery { props: req, filter } if !calendar => {
            let names: Vec<String> =
                objects::list(pool, ctx.actor(), c.id).await?.into_iter().map(|m| m.name).collect();
            let mut ms = Multistatus::default();
            for s in objects::get_many(pool, ctx.actor(), c.id, &names).await? {
                if filter.matches(&s.body) {
                    ms.responses.push(object_response(&c, &s.meta, Some(&s.body), &me, &req));
                }
            }
            multistatus(ms)
        }
        _ => Err(unsupported()),
    }
}

fn far(year: i32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or_default().and_hms_opt(0, 0, 0).unwrap_or_default()
}

async fn multiget(ctx: &Ctx<'_>, c: &Collection, req: &PropRequest, hrefs: &[String]) -> Out {
    let me = ctx.me().to_string();
    // Each href must name an object in this collection; anything else is a
    // 404 entry under the href exactly as sent.
    let mut wanted: Vec<(String, Option<String>)> = Vec::new();
    for h in hrefs {
        let name = match path::parse_href(h) {
            Some(Target::Object { kind, owner, slug, name })
                if kind == props::kind_of(c) && owner == c.owner_name && slug == c.slug =>
            {
                Some(name)
            }
            _ => None,
        };
        wanted.push((h.clone(), name));
    }
    let names: Vec<String> = wanted.iter().filter_map(|(_, n)| n.clone()).collect();
    let found = objects::get_many(&ctx.st.pool, ctx.actor(), c.id, &names).await?;
    let mut ms = Multistatus::default();
    for (href, name) in wanted {
        match name.and_then(|n| found.iter().find(|s| s.meta.name == n)) {
            Some(s) => ms.responses.push(object_response(c, &s.meta, Some(&s.body), &me, req)),
            None => ms.responses.push(DavResponse::Status { href, status: 404 }),
        }
    }
    multistatus(ms)
}
