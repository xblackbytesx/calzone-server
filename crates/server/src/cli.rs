//! Admin commands from the shell (`docker compose exec calzone ...`):
//! bootstrapping the first admin, collections, app passwords, and imports
//! and exports for scripted moves.

use anyhow::{Context, Result, anyhow, bail};
use sqlx::PgPool;

use crate::domain::app_passwords::{self, Scope};
use crate::domain::{Access, CollectionKind, NewCollection, collections, principals};
use crate::domain::{accounts, imports};
use crate::import::{Run, Target, Upload, baikal};

pub const USAGE: &str = "usage:
  calzone-server [serve]
  calzone-server user add NAME [DISPLAY NAME]
  calzone-server user passphrase NAME          (prints a temporary web passphrase)
  calzone-server user admin NAME on|off
  calzone-server user two-factor-reset NAME    (lost phone: off, signed out, set up again)
  calzone-server collection add USER calendar|addressbook SLUG [DISPLAY NAME]
  calzone-server app-password add USER LABEL [--read OWNER/SLUG]... [--write OWNER/SLUG]...
  calzone-server app-password list USER
  calzone-server app-password revoke USER ID
  calzone-server import file USER [--into OWNER/SLUG] FILE...   (.ics, .vcf, Google Takeout .zip)
  calzone-server import baikal USER URL BAIKAL-USER   (password from BAIKAL_PASSWORD or stdin)
  calzone-server export USER FILE.zip
  calzone-server resync                     (after restoring a backup: every phone and DAV client syncs from the start)";

/// An import report as text: counts only, never content.
async fn report(pool: &PgPool, actor: uuid::Uuid, id: uuid::Uuid) -> Result<String> {
    let i = imports::get(pool, actor, id).await?;
    let mut out = format!("import {id}: {}\n", i.status.as_str());
    if !i.message.is_empty() {
        out.push_str(&format!("  {}\n", i.message));
    }
    for c in &i.summary {
        out.push_str(&format!(
            "  {}{}: {} new, {} updated, {} unchanged, {} not imported\n",
            c.name,
            if c.created_collection { " (made)" } else { "" },
            c.created,
            c.updated,
            c.unchanged,
            c.failed
        ));
    }
    for (n, collection, reason) in imports::failures(pool, actor, id).await? {
        out.push_str(&format!("  not imported #{n} in {collection}: {reason}\n"));
    }
    Ok(out.trim_end().to_string())
}

/// Run one admin command; the text to print.
pub async fn run(pool: &PgPool, args: &[&str]) -> Result<String> {
    match args {
        ["user", "add", name, display @ ..] => {
            let id = principals::create_user(pool, name, &display.join(" ")).await?;
            Ok(format!("user {name} created ({id})"))
        }
        ["user", "passphrase", name] => {
            let user = principals::by_name(pool, name).await.context("no such user")?;
            let p = accounts::issue_temporary(pool, user.id).await?;
            Ok(format!(
                "temporary passphrase for {name}:\n\n    {p}\n\n\
                 Shown once. Log in to the web app and change it under Settings."
            ))
        }
        ["user", "admin", name, on] => {
            let user = principals::by_name(pool, name).await.context("no such user")?;
            let admin = crate::config::switch("admin", Some(on), false)?;
            accounts::set_admin(pool, user.id, admin).await?;
            Ok(format!("{name} is {}an admin", if admin { "" } else { "not " }))
        }
        ["user", "two-factor-reset", name] => {
            let user = principals::by_name(pool, name).await.context("no such user")?;
            crate::domain::two_factor::disable(pool, user.id).await?;
            Ok(format!("two-factor sign-in reset for {name}; they set it up again at their next login"))
        }
        ["collection", "add", user, kind, slug, display @ ..] => {
            let kind = match *kind {
                "calendar" => CollectionKind::Calendar,
                "addressbook" => CollectionKind::AddressBook,
                other => bail!("kind must be calendar or addressbook, not {other:?}"),
            };
            let owner = principals::by_name(pool, user).await.context("no such user")?;
            let display = display.join(" ");
            let new = NewCollection {
                kind,
                slug,
                display_name: if display.is_empty() { slug } else { &display },
                description: "",
                color: None,
                components: &[],
            };
            let c = collections::create(pool, owner.id, new).await?;
            Ok(format!("{} {}/{} created", kind.as_str(), c.owner_name, c.slug))
        }
        ["app-password", "add", user, label, rest @ ..] => {
            let owner = principals::by_name(pool, user).await.context("no such user")?;
            let mut scope: Option<Scope> = None;
            let mut it = rest.iter();
            while let Some(flag) = it.next() {
                let level = match *flag {
                    "--read" => Access::Read,
                    "--write" => Access::Write,
                    other => bail!("unknown option {other:?}\n{USAGE}"),
                };
                let target = it.next().ok_or_else(|| anyhow!("{flag} needs OWNER/SLUG"))?;
                let (o, s) = target.split_once('/').ok_or_else(|| anyhow!("{target:?} is not OWNER/SLUG"))?;
                let c =
                    collections::by_path(pool, owner.id, o, s).await.with_context(|| format!("{target} not found"))?;
                scope.get_or_insert_default().insert(c.id, level);
            }
            let (id, password) = app_passwords::create(pool, owner.id, label, scope.as_ref()).await?;
            Ok(format!(
                "app password {id} for {user} ({label}):\n\n    {password}\n\n\
                 Shown once. Sign in with user name {user} at /dav/ (or just the server address)."
            ))
        }
        ["app-password", "list", user] => {
            let owner = principals::by_name(pool, user).await.context("no such user")?;
            let mut out = String::new();
            for p in app_passwords::list(pool, owner.id).await? {
                let scope = match &p.scope {
                    None => "everything".to_string(),
                    Some(s) => format!("{} collection(s)", s.len()),
                };
                let used = p.last_used_at.map(|t| t.format("%Y-%m-%d %H:%M").to_string()).unwrap_or("never".into());
                out.push_str(&format!("{}  {}  scope: {scope}  last used: {used}\n", p.id, p.label));
            }
            if out.is_empty() {
                out.push_str("no app passwords\n");
            }
            Ok(out.trim_end().to_string())
        }
        ["app-password", "revoke", user, id] => {
            let owner = principals::by_name(pool, user).await.context("no such user")?;
            let id = id.parse().context("not an app password id")?;
            app_passwords::revoke(pool, owner.id, id).await?;
            Ok(format!("app password {id} revoked"))
        }
        ["import", "file", user, rest @ ..] => {
            let into = principals::by_name(pool, user).await.context("no such user")?;
            let (target, paths) = match rest {
                ["--into", t, paths @ ..] => {
                    let (o, s) = t.split_once('/').ok_or_else(|| anyhow!("{t:?} is not OWNER/SLUG"))?;
                    let c =
                        collections::by_path(pool, into.id, o, s).await.with_context(|| format!("{t} not found"))?;
                    (Target::Existing(c.id), paths)
                }
                paths => (Target::ByName, paths),
            };
            if paths.is_empty() {
                bail!("{USAGE}");
            }
            let mut uploads = Vec::new();
            for p in paths {
                let bytes = std::fs::read(p).with_context(|| format!("cannot read {p}"))?;
                uploads.push(Upload { filename: p.rsplit('/').next().unwrap_or(p).to_string(), bytes });
            }
            let names: Vec<&str> = paths.iter().map(|p| p.rsplit('/').next().unwrap_or(p)).collect();
            let source = format!("file {} (command line)", names.join(", "));
            let run = Run::begin(pool, into.id, into.id, &source).await?;
            let id = run.files(uploads, target).await;
            report(pool, into.id, id).await
        }
        ["import", "baikal", user, url, remote_user] => {
            let into = principals::by_name(pool, user).await.context("no such user")?;
            let password = match std::env::var("BAIKAL_PASSWORD") {
                Ok(p) if !p.is_empty() => p,
                _ => {
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line).context("reading the Baikal password from stdin")?;
                    line.trim_end_matches(['\r', '\n']).to_string()
                }
            };
            let url = baikal::check_url(url).map_err(|e| anyhow!(e))?;
            let source = format!("Baikal {url} as {remote_user} (command line)");
            let run = Run::begin(pool, into.id, into.id, &source).await?;
            let src = baikal::Source { url: url.to_string(), user: remote_user.to_string(), password };
            let id = run.baikal(src).await;
            report(pool, into.id, id).await
        }
        ["export", user, path] => {
            let owner = principals::by_name(pool, user).await.context("no such user")?;
            let zip = crate::export::everything(pool, owner.id).await?;
            std::fs::write(path, &zip).with_context(|| format!("cannot write {path}"))?;
            Ok(format!("wrote {path} ({} bytes)", zip.len()))
        }
        ["resync"] => {
            let n = collections::start_over(pool).await?;
            Ok(format!("{n} collections renumbered; every phone and DAV client syncs from the start at its next sync"))
        }
        _ => bail!("{USAGE}"),
    }
}
