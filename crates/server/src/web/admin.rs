//! People and groups (admins only). New users and forgotten passphrases get
//! a temporary passphrase, shown once; the person changes it in Settings.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect};
use serde::Deserialize;
use uuid::Uuid;

use super::session::AdminUser;
use super::{Nav, Page, WebError, base_url, html, i18n};
use crate::AppState;
use crate::domain::accounts::{self, Account};
use crate::domain::principals;

pub struct GroupRow {
    pub group: Account,
    pub members: Vec<Account>,
    pub others: Vec<Account>,
}

#[derive(Template)]
#[template(path = "people.html")]
struct PeoplePage {
    nav: Nav,
    me: Uuid,
    users: Vec<Account>,
    groups: Vec<GroupRow>,
}

pub async fn people(State(st): State<AppState>, AdminUser(user): AdminUser) -> Page {
    let all = accounts::list(&st.pool).await?;
    let users: Vec<Account> = all.iter().filter(|a| !a.is_group()).cloned().collect();
    let mut groups = Vec::new();
    for g in all.iter().filter(|a| a.is_group()) {
        let ids = principals::members(&st.pool, g.id).await?;
        let (members, others) = users.iter().cloned().partition(|u| ids.contains(&u.id));
        groups.push(GroupRow { group: g.clone(), members, others });
    }
    html(PeoplePage { nav: Nav::new(&st, &user, "people"), me: user.id(), users, groups })
}

#[derive(Template)]
#[template(path = "passphrase_issued.html")]
struct IssuedPage {
    nav: Nav,
    who: String,
    name: String,
    passphrase: String,
    server: String,
}

#[derive(Deserialize)]
pub struct NewUserForm {
    name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    admin: Option<String>,
}

pub async fn create_user(
    State(st): State<AppState>,
    AdminUser(user): AdminUser,
    headers: HeaderMap,
    Form(f): Form<NewUserForm>,
) -> Page {
    let id = principals::create_user(&st.pool, &f.name, f.display_name.trim()).await?;
    if f.admin.is_some() {
        accounts::set_admin(&st.pool, id, true).await?;
    }
    issued(&st, &user, &headers, id).await
}

async fn issued(st: &AppState, user: &super::session::WebUser, headers: &HeaderMap, id: Uuid) -> Page {
    let account = accounts::get(&st.pool, id).await?;
    if account.is_group() {
        return Err(WebError::bad(i18n::a(user.lang).err_groups_no_signin));
    }
    let passphrase = accounts::issue_temporary(&st.pool, id).await?;
    html(IssuedPage {
        nav: Nav::new(st, user, "people"),
        who: account.label().to_string(),
        name: account.name.clone(),
        passphrase,
        server: base_url(st, headers),
    })
}

pub async fn reset_passphrase(
    State(st): State<AppState>,
    AdminUser(user): AdminUser,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Page {
    if id == user.id() {
        return Err(WebError::bad(i18n::a(user.lang).err_own_passphrase));
    }
    issued(&st, &user, &headers, id).await
}

fn not_yourself(user: &super::session::WebUser, id: Uuid) -> Result<(), WebError> {
    if id == user.id() {
        // So the last admin cannot lock everyone out by accident.
        return Err(WebError::new(StatusCode::BAD_REQUEST, i18n::a(user.lang).err_not_yourself));
    }
    Ok(())
}

pub async fn disable(State(st): State<AppState>, AdminUser(user): AdminUser, Path(id): Path<Uuid>) -> Page {
    not_yourself(&user, id)?;
    accounts::get(&st.pool, id).await?;
    principals::set_disabled(&st.pool, id, true).await?;
    Ok(Redirect::to("/admin/people").into_response())
}

pub async fn enable(State(st): State<AppState>, AdminUser(user): AdminUser, Path(id): Path<Uuid>) -> Page {
    accounts::get(&st.pool, id).await?;
    principals::set_disabled(&st.pool, id, false).await?;
    let _ = user;
    Ok(Redirect::to("/admin/people").into_response())
}

/// Someone lost their phone and recovery codes: two-factor goes off for
/// them and their sessions end; they set it up again at the next login.
pub async fn reset_two_factor(State(st): State<AppState>, AdminUser(user): AdminUser, Path(id): Path<Uuid>) -> Page {
    not_yourself(&user, id)?;
    accounts::get(&st.pool, id).await?;
    crate::domain::two_factor::disable(&st.pool, id).await?;
    Ok(Redirect::to("/admin/people").into_response())
}

#[derive(Deserialize)]
pub struct AdminForm {
    admin: String,
}

pub async fn set_admin(
    State(st): State<AppState>,
    AdminUser(user): AdminUser,
    Path(id): Path<Uuid>,
    Form(f): Form<AdminForm>,
) -> Page {
    not_yourself(&user, id)?;
    accounts::set_admin(&st.pool, id, f.admin == "on").await?;
    Ok(Redirect::to("/admin/people").into_response())
}

#[derive(Deserialize)]
pub struct NewGroupForm {
    name: String,
    #[serde(default)]
    display_name: String,
}

pub async fn create_group(State(st): State<AppState>, AdminUser(_): AdminUser, Form(f): Form<NewGroupForm>) -> Page {
    principals::create_group(&st.pool, &f.name, f.display_name.trim()).await?;
    Ok(Redirect::to("/admin/people").into_response())
}

#[derive(Deserialize)]
pub struct MemberForm {
    member: Uuid,
}

pub async fn add_member(
    State(st): State<AppState>,
    AdminUser(user): AdminUser,
    Path(id): Path<Uuid>,
    Form(f): Form<MemberForm>,
) -> Page {
    // Members are users; a group inside a group would not pass access on.
    let (group, member) = (accounts::get(&st.pool, id).await?, accounts::get(&st.pool, f.member).await?);
    if !group.is_group() || member.is_group() {
        return Err(WebError::bad(i18n::a(user.lang).err_add_people));
    }
    principals::add_member(&st.pool, id, f.member).await?;
    Ok(Redirect::to("/admin/people").into_response())
}

pub async fn remove_member(
    State(st): State<AppState>,
    AdminUser(_): AdminUser,
    Path((id, member)): Path<(Uuid, Uuid)>,
) -> Page {
    principals::remove_member(&st.pool, id, member).await?;
    Ok(Redirect::to("/admin/people").into_response())
}
