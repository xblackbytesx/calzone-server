//! Import and export (decision 30): download everything or one collection,
//! upload `.ics` / `.vcf` / a Google Takeout zip, copy a Baikal account
//! (admins), and follow an import's report. Imports run in a background
//! task; the report page refreshes itself until it is done.

use askama::Template;
use axum::Form;
use axum::extract::{Multipart, Path, State};
use axum::http::HeaderValue;
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use uuid::Uuid;

use super::session::{AdminUser, WebUser};
use super::{Nav, Page, WebError, html, i18n};
use crate::AppState;
use crate::domain::imports::{self, Import, Status};
use crate::domain::{CollectionKind, accounts, collections};
use crate::import::{MAX_UPLOAD, Run, Target, Upload, baikal};

pub struct Choice {
    pub id: Uuid,
    pub label: String,
}

pub struct ImportRow {
    pub id: Uuid,
    pub when: String,
    pub source: String,
    pub into: String,
    pub status: &'static str,
    pub totals: String,
}

#[derive(Template)]
#[template(path = "import.html")]
struct ImportPage {
    nav: Nav,
    targets: Vec<Choice>,
    people: Vec<Choice>,
    me: Uuid,
    recent: Vec<ImportRow>,
}

fn status_words(a: &'static i18n::A, s: Status) -> &'static str {
    match s {
        Status::Running => a.status_running,
        Status::Done => a.status_done,
        Status::Failed => a.status_failed,
        Status::Interrupted => a.status_interrupted,
    }
}

fn totals(a: &'static i18n::A, i: &Import) -> String {
    let t = i.totals();
    let mut parts = vec![
        format!("{} {}", t.created, a.n_new),
        format!("{} {}", t.updated, a.n_updated),
        format!("{} {}", t.unchanged, a.n_unchanged),
    ];
    if t.failed > 0 {
        parts.push(format!("{} {}", t.failed, a.n_not_imported));
    }
    parts.join(", ")
}

pub async fn page(State(st): State<AppState>, user: WebUser) -> Page {
    let mut targets = Vec::new();
    for kind in [CollectionKind::Calendar, CollectionKind::AddressBook] {
        for c in collections::visible(&st.pool, user.id(), kind).await? {
            if !c.access.can_write() {
                continue;
            }
            let name = if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() };
            let what = if kind == CollectionKind::Calendar { "calendar" } else { "address book" };
            let label = if c.owner_id == user.id() {
                format!("{name} ({what})")
            } else {
                format!("{name} ({what}, {})", c.owner_name)
            };
            targets.push(Choice { id: c.id, label });
        }
    }
    let people = if user.account.is_admin {
        accounts::list(&st.pool)
            .await?
            .into_iter()
            .filter(|a| !a.is_group() && !a.disabled)
            .map(|a| Choice { id: a.id, label: format!("{} ({})", a.label(), a.name) })
            .collect()
    } else {
        Vec::new()
    };
    let recent = imports::recent(&st.pool, user.id(), 10)
        .await?
        .iter()
        .map(|i| ImportRow {
            id: i.id,
            when: i.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
            source: i.source.clone(),
            into: i.principal_name.clone(),
            status: status_words(i18n::a(user.lang), i.status),
            totals: totals(i18n::a(user.lang), i),
        })
        .collect();
    html(ImportPage { nav: Nav::new(&st, &user, "import"), targets, people, me: user.id(), recent })
}

/// One import at a time per person, so a double click cannot run two.
async fn none_running(st: &AppState, actor: Uuid, a: &'static i18n::A) -> Result<(), WebError> {
    if imports::recent(&st.pool, actor, 5).await?.iter().any(|i| i.status == Status::Running) {
        return Err(WebError::bad(a.err_import_running));
    }
    Ok(())
}

/// The upload form: `target` (`byname` or a collection id) and one or more
/// `file` parts.
pub async fn upload(State(st): State<AppState>, user: WebUser, mut form: Multipart) -> Page {
    let a = i18n::a(user.lang);
    let bad = |_| WebError::bad(a.err_upload);
    let mut target = Target::ByName;
    let mut uploads = Vec::new();
    while let Some(field) = form.next_field().await.map_err(bad)? {
        match field.name() {
            Some("target") => {
                let v = field.text().await.map_err(bad)?;
                if v != "byname" {
                    let id = v.parse::<Uuid>().map_err(|_| WebError::bad(a.err_choose_target))?;
                    target = Target::Existing(id);
                }
            }
            Some("file") => {
                let filename = field.file_name().unwrap_or("").to_string();
                let bytes = field.bytes().await.map_err(bad)?;
                if !filename.is_empty() || !bytes.is_empty() {
                    uploads.push(Upload {
                        filename: if filename.is_empty() { "upload".into() } else { filename },
                        bytes: bytes.to_vec(),
                    });
                }
            }
            _ => {}
        }
    }
    if uploads.is_empty() {
        return Err(WebError::bad(a.err_choose_file));
    }
    if let Target::Existing(id) = target {
        let c = collections::get(&st.pool, user.id(), id).await?;
        if !c.access.can_write() {
            return Err(WebError::bad(a.err_cannot_add));
        }
    }
    none_running(&st, user.id(), i18n::a(user.lang)).await?;
    let names: Vec<&str> = uploads.iter().map(|u| u.filename.as_str()).collect();
    let source = format!("file {}", names.join(", ")).chars().take(300).collect::<String>();
    let run = Run::begin(&st.pool, user.id(), user.id(), &source).await?;
    let id = run.id;
    tokio::spawn(run.files(uploads, target));
    Ok(Redirect::to(&format!("/import/{id}")).into_response())
}

#[derive(Deserialize)]
pub struct BaikalForm {
    into: Uuid,
    url: String,
    user: String,
    password: String,
}

/// Copy a Baikal account into a CalZone account (admins: the importer
/// makes outgoing requests, and may fill someone else's account).
pub async fn baikal(State(st): State<AppState>, AdminUser(admin): AdminUser, Form(f): Form<BaikalForm>) -> Page {
    let url = baikal::check_url(&f.url).map_err(WebError::bad)?;
    if f.user.trim().is_empty() || f.password.is_empty() {
        return Err(WebError::bad(i18n::a(admin.lang).err_baikal_fields));
    }
    let into = accounts::get(&st.pool, f.into).await?;
    if into.is_group() || into.disabled {
        return Err(WebError::bad(i18n::a(admin.lang).err_enabled_person));
    }
    none_running(&st, into.id, i18n::a(admin.lang)).await?;
    none_running(&st, admin.id(), i18n::a(admin.lang)).await?;
    let source = format!("Baikal {url} as {}", f.user.trim());
    let run = Run::begin(&st.pool, into.id, admin.id(), &source).await?;
    let id = run.id;
    tokio::spawn(run.baikal(baikal::Source {
        url: url.to_string(),
        user: f.user.trim().to_string(),
        password: f.password,
    }));
    Ok(Redirect::to(&format!("/import/{id}")).into_response())
}

pub struct CountRow {
    pub id: Option<Uuid>,
    pub name: String,
    pub new_collection: bool,
    pub created: u32,
    pub updated: u32,
    pub unchanged: u32,
    pub failed: u32,
}

pub struct FailureRow {
    pub position: i32,
    pub collection: String,
    pub reason: String,
}

#[derive(Template)]
#[template(path = "import_report.html")]
struct ReportPage {
    nav: Nav,
    i: Import,
    status: &'static str,
    running: bool,
    totals: String,
    rows: Vec<CountRow>,
    failures: Vec<FailureRow>,
    finished: String,
}

pub async fn report(State(st): State<AppState>, user: WebUser, Path(id): Path<Uuid>) -> Page {
    let i = imports::get(&st.pool, user.id(), id).await?;
    let failures = imports::failures(&st.pool, user.id(), id)
        .await?
        .into_iter()
        .map(|(position, collection, reason)| FailureRow { position, collection, reason })
        .collect();
    let rows = i
        .summary
        .iter()
        .map(|c| CountRow {
            id: c.collection_id,
            name: c.name.clone(),
            new_collection: c.created_collection,
            created: c.created,
            updated: c.updated,
            unchanged: c.unchanged,
            failed: c.failed,
        })
        .collect();
    let finished = i.finished_at.map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string()).unwrap_or_default();
    html(ReportPage {
        nav: Nav::new(&st, &user, "import"),
        status: status_words(i18n::a(user.lang), i.status),
        running: i.status == Status::Running,
        totals: totals(i18n::a(user.lang), &i),
        rows,
        failures,
        finished,
        i,
    })
}

fn download(filename: &str, content_type: &'static str, body: Vec<u8>) -> Response {
    // File names are slugs or numbers; keep the header plain ASCII anyway.
    let safe: String =
        filename.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")).expect("ascii header");
    (
        [
            (CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (CONTENT_DISPOSITION, disposition),
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response()
}

/// The bytes of an entry that was not imported, as received. Always a
/// download, never rendered (it came from outside).
pub async fn raw(State(st): State<AppState>, user: WebUser, Path((id, n)): Path<(Uuid, i32)>) -> Page {
    let bytes = imports::failure_raw(&st.pool, user.id(), id, n).await?;
    Ok(download(&format!("not-imported-{n}.txt"), "application/octet-stream", bytes))
}

pub async fn export_all(State(st): State<AppState>, user: WebUser) -> Page {
    let zip = crate::export::everything(&st.pool, user.id()).await?;
    let date = chrono::Utc::now().format("%Y-%m-%d");
    Ok(download(&format!("calzone-{}-{date}.zip", user.account.name), "application/zip", zip))
}

pub async fn export_one(State(st): State<AppState>, user: WebUser, Path(id): Path<Uuid>) -> Page {
    let (name, ct, text) = crate::export::collection(&st.pool, user.id(), id).await?;
    Ok(download(&name, ct, text.into_bytes()))
}

/// The upload route's body limit (the rest of the web app keeps 64 KiB).
pub const UPLOAD_LIMIT: usize = MAX_UPLOAD;
