# CalZone server: agent guide and decision register

CalZone is a self-hosted calendar and contacts system for a household: shared
calendars from day one, a web app that feels like a real app, a native
Android app that replaces DAVx5 and Fossify Calendar, and scoped access for
the Hermes agent. No big tech, no tracking.

This repo holds the shared core, the server (storage, sharing, the web app,
the sync API, the optional CalDAV/CardDAV door, the MCP door) and the FFI
crate the Android app runs the core through. The Android app will live in
`/workspace/calzone` (its own `AGENTS.md`). This file is the contract for
both repos.

Record decisions here when they are made, with the date and the reason, and
keep it true to the code: mark what is planned as planned. Never use em
dashes. The phase 1 plan is `docs/plan-phase-1.md`; phase 2 is `docs/plan-phase-2.md`.

## Status

2026-11-06: contract and phase 1 plan written; spike S1 decided (decision
25). Phase 1 step 1 done: the workspace builds, `make` targets, the dev and
smoke-test compose files, CI and release workflows (Gitea then, GitHub since decision 22 was revised), `deploy/`. The
server runs its migrations and answers `/healthz`, and `CALZONE_DAV` is
parsed. Verified with fmt, clippy `-D warnings`, 4 tests (2 against a real
Postgres 17) and a run of the binary.

Phase 1 step 2 done (2026-11-06): `calzone-core` has `lines` (lossless
content-line tree), `object` (resource rules, projection), `recur`
(expansion with decision 25's four fixes), `vtimezone` (VTIMEZONE rule
evaluation) and `split` (the per-user layer). 41 core tests, including a
corpus test over the synthetic fixtures and the household corpus:
185 objects and 1412 cards round-trip byte for byte, all 36 recurring
events agree with the independent `rrule` crate, and 59 alarms split out
and merge back exactly. Checks for `aarch64-linux-android` and
`x86_64-linux-android`. The dav crate is still empty.

Phase 1 step 3 done (2026-11-06): migration `0002_storage.sql` and the
domain service (`crates/server/src/domain/`). 14 server tests on Postgres,
including concurrency and the household corpus through the real store
(185 events and 1412 cards read back byte for byte, alarms in 43 owner
layers, none in shared bodies).

Phase 1 step 4 done (2026-11-06): the CalDAV/CardDAV door (decision 28).
`calzone-dav` parses and writes the protocol; `server::dav` authenticates
app passwords (migration `0003_app_passwords.sql`) and calls the domain.
Admin CLI for users, collections and app passwords. 12 door tests on
Postgres replay DAVx5 and Thunderbird request shapes; the household corpus
goes through PUT and a sync REPORT byte for byte; the python `caldav`
library passes `tests/interop/caldav_client.py` against the real binary
(also a CI step). Device matrix (DAVx5, Fossify, Thunderbird) not run yet.

Phase 1 step 5 done (2026-11-06): the admin pages (decision 29). Login with
Argon2id passphrases and sessions (migration `0004_web.sql`), Calendars &
contacts (create, edit, share, typed delete), Devices (app passwords),
People (users, admins, groups), Settings. 7 page tests on Postgres plus a
template check that nothing inline breaks the CSP; a headless Chromium
pass over every page at phone and desktop width, light and dark, found no
CSP violations, script errors or horizontal overflow. Import and export
pages come with step 6. 102 workspace tests.

Phase 1 step 6 done (2026-11-06): import and export (decision 30). Core
`import` (split folds repaired, `.ics`/`.vcf` split per UID, files
assembled back), `objects::upsert` (unchanged objects are not written),
import reports (migration `0005_imports.sql`), the Baikal reader (PROPFIND
and REPORT only, Basic or Digest), file uploads including a Takeout zip,
the zip and per-collection exports, the Import & export page and CLI
commands. 125 workspace tests: the Baikal reader against our own door over
TCP (Basic and a Digest gate), re-runs that change nothing, and the
household export files through import and export (185 events and 1412
cards; all 1597 unchanged on a second run; the export gives back every
component). A real Baikal and a real Takeout file are not tried yet.

Phase 1 step 7 done (2026-11-06): deploy and hand-off (decision 31). The
server stops on SIGTERM (it ignored `docker compose down` before);
`deploy/README.md` covers backups, restore and upgrades;
`docs/leaving-baikal.md` is the move-over procedure. Phase 1 is built; what
remains are the exit criteria that need the household: the device matrix, a
real Baikal copy run twice, a week of use, and the restore drill.

2026-11-06, going public: CI and releases moved to GitHub (decision 22
revised), and the web login got two-factor sign-in (decision 32,
migration `0006_two_factor.sql`, `CALZONE_SECRET_KEY` now required), and
"Keep me signed in" (decision 33, migration `0007_session_length.sql`),
and one `auth_failure` log line per refused sign-in with a CrowdSec parser
and scenario in `deploy/crowdsec/` (decision 34). 134 workspace tests.

Phase 2 started (2026-11-06): spike S2 decided (decision 26, EventCalendar
for the grid); the plan is `docs/plan-phase-2.md`. Step 1 done the same
day: core `moment`, `edit` (lossless, all three recurring scopes) and
`quickadd` (English and Dutch); the household corpus passes every edit
check. 153 workspace tests. Step 2 done the same day: the calendar API
(decision 37), migration `0008_calendar_app.sql`. 159 workspace tests.
Step 3 done the same day: the calendar page (decision 38), checked in
headless Chromium against the real binary (drag, scope, undo, panel,
delete, quick add, keys, phone, light, Dutch). 161 workspace tests.
Step 4 done the same day: contacts (decision 39); all 1412 household cards
edit losslessly; checked in Chromium with 1500 synthetic contacts. 171
workspace tests.

## Architecture

```
 Thunderbird, DAVx5 ──► CalDAV/CardDAV door (optional) ──┐
 Android app ─────────► sync API door ───────────────────┤
 Browser ─────────────► web app (HTMX + grid island) ────┼──► domain service ──► Postgres
 Hermes, scripts ─────► MCP door (scoped tokens) ────────┘     (sharing, ACL,     (objects,
                                                                change log)        change log)
              core crate (model, iCal/vCard, recurrence, merge) is used by all of
              the above and, through crates/ffi, by the Android app
```

One source of truth (the server), several doors into it, one shared core,
two repos.

## Crates (scaffolded; filled in by phase 1 steps 2 to 4)

| Crate | Role | Rule |
|---|---|---|
| `crates/core` (`calzone-core`) | domain model, iCalendar/vCard parse and serialize, recurrence expansion, per-user layer split, sync types, three-way merge | Pure: no IO, no clock (time is passed in), no randomness |
| `crates/dav` (`calzone-dav`) | CalDAV/CardDAV/WebDAV paths, request bodies, query filters, multistatus XML | Pure like the core: no IO; the server's `dav` module does auth and calls the domain (decision 28) |
| `crates/server` (`calzone-server`) | axum + sqlx Postgres + askama/HTMX: domain service, storage, auth, web app, sync API, MCP, push | Runtime sqlx queries only (no `query!`), so builds never need a database |
| `crates/ffi` (`calzone-ffi`) | UniFFI bindings for the app | Thin: JSON in and out (the core's own shapes), `flat_error` for real errors |

Server modules: `config`, `cli` (admin commands), `client` (client
address), `audit` (the `auth_failure` line), `throttle` (login failure budgets), `domain` (`access`,
`principals`, `collections`, `objects`: put, get, get_many, list, in_range,
delete, changes_since; `app_passwords`: create, list, revoke, verify;
`accounts`: passphrases, admins, sessions; `imports`: reports;
`two_factor`: TOTP, recovery codes, secrets at rest; `prefs`: settings;
`events`: occurrences, writes, undo; `contacts`: list, detail, writes,
photos), `import`
(`Run`: files and Takeout zips; `baikal`: the CalDAV/CardDAV reader;
`digest`), `export` (zip, one collection), `dav` (`auth`: Basic; `props`:
live properties; handlers), `web` (`session`, `pages`, `collections`,
`passwords`, `admin`, `transfer`, `two_factor`, `api` (JSON for the
calendar page), `calendar` (the page and the event panel), `contacts`,
`i18n`
(languages and every string); askama templates in `templates/`, assets in
`static/`), `migrations/` (numbered SQL, embedded, run at every start).

Dav modules: `xml` (namespaced tree, escaping), `path` (URL space, hrefs),
`request` (PROPFIND, PROPPATCH, MKCOL/MKCALENDAR, REPORT bodies), `filter`
(calendar-query and addressbook-query), `response` (multistatus, errors).

Core modules: `lines` (Document, Component, Item, Line; fold), `object`
(inspect_calendar, inspect_card, Bounds, unescape), `recur` (expand, When,
Occurrence, Warning), `vtimezone` (VZone), `split` (split, merge,
UserLayer), `import` (decode, split_calendar, split_cards, assemble),
`moment` (Moment, Key, VTIMEZONE generation), `edit` (EventChange, Scope,
edit, delete, new_event), `quickadd` (parse, Lang), `card` (read, apply,
new_card). Per-user items: VALARM, `X-MOZ-LASTACK`, `X-MOZ-SNOOZE-TIME`
and `X-MOZ-SNOOZE-TIME-*`; `X-MOZ-GENERATION` stays shared.

Pins follow wordwarz-server unless a decision here says otherwise: rust 1.92,
axum 0.8, sqlx 0.8, askama 0.16, reqwest 0.13 on rustls/ring, Postgres 17,
HTMX 2.0.4 vendored (`crates/server/static/htmx.min.js`, sha256
`e209dda5c8235479f3166defc7750e1dbcd5a5c1808b7792fc2e6733768fb447`, the
same file as Converge), UniFFI 0.32.

## Build and verify

Everything runs through `make` (docker compose); no host toolchain is needed.

| Target | What |
|---|---|
| `make dev` | bacon hot reload + Postgres 17 on `${PORT:-8080}` |
| `make test` | `cargo test --workspace --release`, Postgres integration tests included |
| `make lint` | `cargo fmt --check` + `clippy -D warnings` (the CI gate) |
| `make image` / `up` / `down` / `logs` | build and smoke-test the production image locally (reads `./.env`) |
| `make android-core` | phase 4 |

CI: `.github/workflows/ci.yml` runs fmt, clippy, the tests (Postgres
service), the Dockerfile manifest check and the python caldav interop on
every push. `release.yml` on a `vX.Y.Z` tag runs the same gate, checks the
tag equals `[workspace.package] version`, pushes the amd64 + arm64 image to
`ghcr.io/xblackbytesx/calzone-server` and creates the GitHub release. No
secrets needed beyond the built-in `GITHUB_TOKEN`.

Agent sandbox fallback (no Docker): rustup toolchain `1.92.0`, a scratch
`CARGO_TARGET_DIR` outside the repo, and a throwaway Postgres 17 from the
zonky embedded-postgres binaries; point `CALZONE_TEST_DATABASE_URL` at its
`postgres` database. Integration tests create and drop their own database
and print `SKIP` when the variable is unset. Never relax a test to make it
pass.

## Decisions

Settled 2026-11-05/06 (do not reopen):

1. **Build our own, in two repos.** `calzone-server` (this repo, Rust + HTMX)
   and `calzone` (Android, Kotlin + Compose, kagami/slopstream/wordwarz
   conventions, no Play Services). GPL-3.0-or-later for both, so DAVx5's
   GPLv3 code (synctools) can be read and borrowed for the Android bridge
   with attribution. Reason: Baikal has no calendar or contacts web app and
   weak sharing; SOGo and Nextcloud do not give the household the app it
   wants.
2. **The server is the single source of truth.** Every client, our own
   included, reaches the data through a door on the domain service. Doors are
   adapters: none touches the database directly.
3. **Our own clients do not speak CalDAV/CardDAV.** The web app calls the
   domain service in-process; the Android app uses the sync API.
   CalDAV/CardDAV is a compatibility door for other apps (Thunderbird, and
   DAVx5 + Fossify until the Android app replaces them). It is switchable per
   instance (`CALZONE_DAV`, default on) and the rest of the system must work
   with it off.
4. **The door ships first anyway.** Phase 1 builds the CalDAV/CardDAV door so
   the household can leave Baikal with the phone apps it already uses, before
   the Android app exists. Each phase replaces one piece in use today (see
   Roadmap).
5. **Objects stay standard and lossless.** Every event, task and contact is
   stored as its complete iCalendar or vCard text plus an indexed projection
   (UID, type, start, end, recurrence bounds, summary, names) for queries. A
   property, parameter or component CalZone does not understand is kept as
   it came and written back unchanged. `.ics`/`.vcf` export of everything is
   always available, so nobody is locked in to our own apps.
6. **One change log.** Every write bumps a per-collection sequence number in
   the same transaction. CalDAV sync tokens, CTags and ETags, and the sync
   API's state tokens are all derived from it, so no two doors can disagree
   about what changed. Deletes leave tombstones in the log.
7. **Multi-user from day one.** Principals are users and groups (a
   household group). Calendars and address books are owned by a principal;
   grants give another principal `read`, `write` or `admin`. The schema
   carries grants from the first migration even where the UI for them comes
   later.
8. **A per-user layer over shared data.** On a calendar someone else can
   see, each user keeps their own colour, display name, visibility and
   reminders. Alarms (VALARM) are always per user: they are taken out of the
   object when it is written and stored for the writing user, and the
   reading user's own alarms are put back when the object is served. My
   reminder never rings on your phone.
9. **Recurrence is expanded by the core only.** The server, the web app's
   grid, the sync API and the Android app all get occurrences from the same
   core code; no client interprets RRULE itself. Timezones come from the IANA
   database, with the object's own VTIMEZONE as the fallback for an unknown
   TZID. Floating times and all-day dates stay what they are.
10. **Accounts and tokens.**
    - Users log in to the web app with a passphrase (Argon2id). Accounts
      are created by an admin; no self sign-up.
    - Phones get per-device tokens through a QR sign-in shown in the web
      app (the wordwarz device-link pattern). Nothing to paste.
    - Personal access tokens for Hermes and scripts are created in the web
      app, each scoped to named collections with `read` or `write`.
    - DAV clients use app passwords, scoped like PATs, only while the DAV
      door is on.
    - Every token and password is revocable on its own. Only hashes are
      stored. Login answers the same for every failure and always pays one
      Argon2 verification.
11. **Web app: HTMX shell plus one grid island.** Pages, forms, the event
    panel, contacts, settings and sharing are HTMX (`hx-boost`, View
    Transitions, preloading neighbouring ranges). The day/week/month time
    grid is a client-side component, because drag to move, drag to resize,
    drag to create and swiping must answer under the finger with no round
    trip. The grid gets expanded occurrences as JSON from the domain service,
    applies a change at once, sends it, and snaps back with a notice if the
    server refuses. Clicking an event opens an HTMX panel. The component is
    chosen in phase 1 spike S2; vkurko EventCalendar is the lead candidate,
    Schedule-X is out (drag and resize are paid since v4).
12. **The web app must be easy enough for the household without help.**
    Requirements: recurring edits always offer "only this one", "this and
    following" and "all"; quick add parses text like "dentist thu 3pm";
    keyboard shortcuts (`t` today, `n` new, arrows or `j`/`k` to page);
    who owns an event is visible through colour and an avatar; undo after
    every move or delete instead of confirm dialogs. It does not ship until
    a non-technical household member has used it without help.
13. **CSS: the shared semantic design system.** Same rules as wordwarz-server
    decision 29 and Converge: layered `app.css`, tokens with `light-dark()`,
    semantic classes only (no utility classes), mobile first, logical
    properties. The grid component is styled with our tokens so it does not
    look like a stock widget.
14. **Android: two jobs in one app.**
    - The system bridge replaces DAVx5. It registers a CalZone account
      type with sync adapters for calendars and contacts, mirrors data into
      CalendarContract and ContactsContract so the dialer, messengers,
      widgets and other calendar apps see it, and turns rows another app
      marked dirty or deleted into pending changes.
    - The calendar UI replaces Fossify and reads CalZone's own store.
    - No contacts app: the system Contacts app edits contacts and the
      bridge syncs them.
    - The local store and the pending-change queue are owned by the core
      through FFI.
15. **The sync API, not CalDAV, for our own app.** Batched changes since a
    state token, each change carrying its base revision, merged by the core
    with a three-way merge per property: concurrent edits to different
    fields both survive. When both sides changed the same field, the server
    wins and the losing value is reported to the user. JMAP's state and
    changes semantics are borrowed, the JMAP spec is not implemented (JMAP
    Calendars is still a draft).
16. **Push is UnifiedPush to declared hosts, encrypted** (RFC 8291, reusing
    wordwarz-server's `webpush.rs` and its rules: declared hosts only, no
    redirects, `404`/`410` drops the endpoint, the body names only what
    changed). WebDAV-Push is not planned: it is a draft and our own app gets
    push directly.
17. **Hermes through a built-in MCP door** with scoped PATs (decision 10),
    not an external CalDAV MCP server holding a full password.
18. **Clients we test against:** DAVx5 + Fossify Calendar on GrapheneOS, and
    Thunderbird. No Apple clients: nobody in the household uses them.
19. **Leaving Baikal is reversible.** The importer reads Baikal over CalDAV
    and CardDAV with read-only requests, is idempotent by UID, and can be
    re-run until the switch. Baikal keeps running until CalZone has been in
    daily use.
20. **Privacy:** no third-party scripts, fonts, analytics or crash
    reporting; the CSP allows only `'self'`. The server talks to nothing but
    the declared push hosts, the import source during an import, and the
    feeds people subscribed to (decision 24).
21. **Out of scope until phase 5:** scheduling and invitations (iTIP/iMIP,
    RFC 6638), free/busy, email of any kind.

Settled 2026-11-06:

22. **CI and releases on GitHub** (moved from the Gitea instance on
    2026-11-06: CalZone has to be reachable from outside the house anyway,
    because a work machine cannot join NetBird, so nothing is gained by
    keeping the code in-house). Public repo
    `github.com/xblackbytesx/calzone-server`, workflows in
    `.github/workflows/` in the wordwarz-server shape: a gate on every push
    (fmt, clippy, tests with a Postgres service, python caldav interop),
    and on a `vX.Y.Z` tag the same gate, a check that the tag equals
    `[workspace.package] version`, the public image
    `ghcr.io/xblackbytesx/calzone-server` (amd64 + arm64) and a GitHub
    release. Rust comes from rustup reading `rust-toolchain.toml`. Going
    public is also why the web login gets two-factor auth before the first
    release.
23. **The production example lives in this repo's `deploy/`** (Traefik,
    Postgres, `.env.example`, README), for now, not in privacybox.
24. **ICS feeds, both directions.** The household used to share through
    calendar subscription links, and a person moving over from Google
    Calendar can bring theirs, so plain `.ics` must work alongside CalDAV.
    - **Publishing (phase 2):** any calendar can get a secret subscription
      link (`webcal://` / `https://`), read-only, for one calendar. Each
      link is a revocable token (only its hash stored) with a detail level:
      full, or busy-only (times without titles, places or notes). Alarms
      and per-user layers are never in a feed. The link works with the DAV
      door off.
    - **Subscribing (phase 2):** a user adds an external `.ics` URL, such
      as a Google Calendar secret address during a move, school holidays or
      public holidays. It becomes a read-only calendar refreshed on a
      schedule. Each fetch is checked against SSRF: no private, loopback
      or link-local addresses unless the instance allows them, redirects
      re-checked, and capped size and time. A failed or empty refresh keeps
      the last good copy; a feed is replaced only by a newer copy that
      parsed completely.
    - **Files (phase 1):** `.ics` and `.vcf` import and export, decision 5
      and the phase 1 plan, so a Google Takeout export imports directly.

25. **S1 result (2026-11-06): our own content-line layer for storage, calcard
    0.3.14 for meaning.**
    - **Measured** against the household's Baikal export (185 events, 36
      recurring, 2 timezones; 1412 DAVx5 vCard 4.0 cards) and synthetic
      cases:
      - calcard parsed everything.
      - vCards came back with no loss at all.
      - 4 RRULE lines came back rewritten (same meaning).
      - Recurrence expansion matched the `rrule` crate on all 36 recurring
        events, and was right across DST and for Feb 29.
    - **The `icalendar` crate is out:** it changed SUMMARY, DESCRIPTION and
      LOCATION on 3 events and split lines on write.
    - **Storage and edits use our own small content-line tree** (components,
      raw property lines, unfold/fold). Unchanged lines stay byte-identical
      by construction, and the per-user split (decision 8) moves whole
      lines. calcard is used only to read meaning: dates, the projection,
      recurrence expansion. Its writer is never used for stored objects.
    - **Dependency:** `calcard = "=0.3.14"`, `default-features = false` (no
      JMAP). Apache-2.0 OR MIT, pure Rust, builds on 1.92. Upstream stopped
      publishing on 2026-10-26, so if we need a fix we vendor this version
      (the licence allows it) and never take code from the AGPL successor.
    - **Our wrapper corrects four calcard behaviours, each pinned by a
      test (`crates/core/src/recur.rs`):**
      - Occurrences of a UTC (`Z`) recurring event come back labelled
        floating; relabel them UTC.
      - An unknown TZID is resolved only by name (`TZID`,
        `X-LIC-LOCATION`, MS CDO ids), never from the VTIMEZONE's rules;
        evaluate the VTIMEZONE ourselves.
      - Its expansion limit is a count shared across the whole calendar;
        expand one event at a time and stop at the requested range.
      - It matches an override to its instance by SEQUENCE as well as
        RECURRENCE-ID, so an override with a bumped SEQUENCE (normal for
        Thunderbird and Google) left the original instance in place: a
        duplicate. Found in step 2 with a synthetic Thunderbird fixture. The
        copy calcard reads has no SEQUENCE lines; dates never depend on it.
27. **Storage and the domain service (step 3, 2026-11-06).**
    - **Doors call `domain::*` only.** Every function takes the actor (a
      principal id) and checks access first; anything unseen is
      `NotFound`. A read grant gives `ReadOnly` on writes; a disabled
      principal reaches nothing.
    - **Access** is the best of: owner, a direct grant, or a grant to a group
      the actor is in. Grants are `read`, `write` or `admin` (admin may
      rename, recolour, share and delete). The owner cannot be granted.
    - **Writes** lock the collection row (`FOR UPDATE`), check `If-Match` /
      `If-None-Match`, validate with the core, bump the collection
      sequence and append to `changes`, all in one transaction.
    - **The ETag is `"<revision>"`,** the collection sequence of the last
      write. Every PUT bumps it, even one that only changed the writer's
      own alarms, so each reader's view always has a fresh ETag when it
      changes.
    - **UIDs are unique per collection** (CalDAV `no-uid-conflict`). A card
      without UID is keyed as `name:<resource name>`.
    - **`changes` keeps every row** (pruning is a later decision). A full
      sync (no token) lists what exists and skips tombstones; a token
      newer than the collection is `BadSyncToken`.
    - **`in_range` returns candidates** from the widened projection, never
      missing one; exact answers expand the candidates with the core.
    - **Credentials moved to step 4** (DAV app passwords) and step 5 (web
      passphrases), so their tables arrive with the code that uses them.
28. **The DAV door (step 4, 2026-11-06).**
    - **Split:** `calzone-dav` is pure (parse, filter, render XML);
      `server::dav` does auth, resolves collections and calls `domain::*`.
      The protocol crate cannot depend on the server, and pure code is
      testable without a database.
    - **URLs:** `/dav/principals/<user>/`, `/dav/calendars/<user>/` (home),
      `/dav/calendars/<owner>/<slug>/<name>`, the same under
      `/dav/addressbooks/`. A user's home lists every collection they can
      see, shared ones at the owner's path, so each collection has one URL.
      Other users' homes and principals are 404. If a client in the device
      matrix cannot handle a non-child href, add per-user aliases then.
      `/.well-known/caldav|carddav` redirect (301) to `/dav/`, and PROPFIND
      on `/` answers as the root (python caldav and some clients start
      there); every other method on `/` belongs to the web app.
    - **App passwords:** 25 characters from a 32-letter alphabet (125 bits),
      stored as SHA-256 of the normalised form (case, spaces and dashes
      ignored). Not Argon2: the secret is random, and DAV clients send it
      on every request. Optional scope: per-collection `read` or `write`,
      capping the user's own access; it is enforced at one place
      (`Ctx::collection` and `Ctx::visible`), and a scoped password cannot
      create collections. Every failure is the same `401`; ten in 15
      minutes from one address give `429` until the window ends. The address
      is the socket peer, or the right-most `X-Forwarded-For` entry when
      `CALZONE_BEHIND_PROXY=on` (declared, never guessed).
    - **Admin CLI** (`calzone-server user|collection|app-password ...`)
      until the admin pages of step 5. The password is printed once.
    - **Sync tokens** are `urn:calzone:sync:<collection id>:<sequence>`, so
      a token from a deleted and recreated collection is refused. The CTag
      is the same string.
    - **Privileges:** read; write adds `write-content`, `bind`, `unbind`;
      admin adds `write` and `write-properties`; the owner also `all`.
    - **Properties:** PROPPATCH sets display name, description and colour
      only, all or nothing (403 plus 424). MKCALENDAR also takes the
      component set, and accepts and drops `calendar-timezone(-id)` and
      `calendar-order` (DAVx5 sends them; objects carry their own zones).
      Address books advertise vCard 3.0 only: cards are stored as sent.
    - **Queries:** component, time range and property filters
      (defined, not defined, text match). Anything else is `403
      supported-filter`. A time range uses the projection, then expands a
      recurring event in a closed range, widened by the floating margin.
      `expand` in `calendar-data` is not honoured: objects are served
      whole.
    - **Depth:** a missing header is 0, infinity is refused
      (`propfind-finite-depth`). Limits: 10 MiB per object, 4 MiB per XML
      body. PUT returns the ETag, since the writer reads back exactly what
      it wrote.
29. **The admin pages (step 5, 2026-11-06).**
    - **Logins:** a user name and an Argon2id passphrase (at least 10
      characters). Every failure (unknown name, group, disabled, no
      passphrase yet, wrong) gives the same page and pays one Argon2
      verification. Budgets: 10 failures per address and 5 per account in
      15 minutes, checked before Argon2 runs; a good login clears the
      account's count, never the address's.
    - **No typed passwords from admins:** a new user, or a forgotten
      passphrase, gets a temporary one generated by the server (the app
      password format) and shown once; the person changes it in Settings.
      The first admin comes from the CLI (`user add`, `user admin NAME on`,
      `user passphrase NAME`). Setting a passphrase ends every session of
      that user; Settings then starts a fresh one in the browser that did
      it. Admins cannot disable or demote themselves, so the last admin
      cannot lock everyone out by accident.
    - **Sessions:** 256 random bits in an `HttpOnly`, `SameSite=Strict`
      cookie (`Secure` unless `CALZONE_COOKIE_SECURE=off`), stored as
      SHA-256 in `web_sessions`, 30 days. A disabled user's sessions stop
      at once. Sessions are domain functions like everything else.
    - **Pages:** Calendars & contacts (the user's own and shared ones;
      create with a slug made from the name, numbered when taken; rename,
      describe, recolour, share with a person or group as view, edit, or
      edit and share; delete only with the name typed). Devices (app
      passwords, scoped per collection, shown once with the server address
      and user name; hidden when the DAV door is off). People (admins:
      add, new passphrase, admin on or off, disable or enable; groups and
      their members, users only). Settings (display name, passphrase).
    - **Security:** the Converge guard: a cross-site `Origin` on any write
      is refused, CSP `default-src 'none'` with `self` scripts and styles,
      `nosniff`, `same-origin` referrers. No inline script or style; a
      test reads every template. Calendar colours are SVG `fill`
      attributes, which the CSP allows. Errors arrive as a flash fragment
      (HX-Retarget) or, without JavaScript, as a small page.
    - **HTMX:** `hx-boost` on the body and plain forms that redirect after
      success, so every page also works without JavaScript; confirmations
      use `hx-confirm`. The CSS is Converge's layers trimmed to what these
      pages use (decision 13).
    - **`/`** is the web app's (GET goes to Calendars & contacts or the
      login); with the DAV door on, its fallback answers PROPFIND.
    - **Import and export** pages arrive with step 6, next to the code that
      does the work.
30. **Import and export (step 6, 2026-11-06).**
    - **One path for every source:** files and Baikal both go through
      `objects::upsert` as the importing account, so its alarms land in its
      own layer. An object with the same UID is updated in place under its
      existing name; nothing is written when the account would read back
      exactly the incoming bytes, so a re-run changes nothing (no sequence
      bump, phones download nothing).
    - **Nothing is dropped:** an object the store refuses, a second object
      with a UID already written in the same run, a file that is not text,
      and an entry the source listed but did not send are kept in
      `import_failures` as the bytes received, downloadable from the
      report. Reports are visible to the target account and the starter.
    - **Files:** `.ics`, `.vcf` and zips (Google Takeout keeps one `.ics`
      per calendar); kind is sniffed from the content. An `.ics` becomes one
      object per UID with the VTIMEZONEs it references (unreferenced ones
      are not kept) and without `METHOD` (RFC 4791 forbids it in stored
      objects); other calendar properties stay. Cards are kept byte for
      byte. "Named after the file" uses the account's own collection whose
      slug comes from `X-WR-CALNAME` (or the file name) and makes it when
      missing, so the same file lands in the same place. Uploads up to
      64 MiB; a zip may unpack to 256 MiB and 1000 files.
    - **The UTF-8 fold gap (from step 2):** a fold that splits a UTF-8
      character (an incomplete character before it, a continuation byte
      after it) is removed before decoding, also inside XML (`&#13;`
      folds). Bytes that are still not UTF-8 are never guessed at (no
      Latin-1 fallback): the file is kept whole as a failure.
    - **Baikal:** admins only (the server makes outgoing requests and may
      fill someone else's account), plus the CLI. Only PROPFIND and REPORT
      exist in the reader's method type. Discovery: principal, homes,
      collections, members, then multigets of 50; a batch that does not
      parse is asked again one entry at a time. Auth: the first 401's
      challenge picks Digest (MD5, `qop=auth`) over Basic; redirects are
      followed by hand on the same host only; the password lives only for
      the run and is never stored or logged. Each remote collection maps to
      the account's own collection with the same slug and kind (made when
      missing, with its name, description, colour and components);
      resource names are kept.
    - **Imports run in a background task**; the report page refreshes
      itself while running. One import at a time per account. Imports still
      running at startup are marked interrupted (a re-run converges).
    - **Export:** one collection as `.ics`/`.vcf` from its page, or a zip of
      everything reachable (own at the top, shared under
      `shared/<owner>/`), served as the downloader reads it, so their own
      alarms are in and nobody else's. A calendar file gets `VERSION`,
      `PRODID`, `X-WR-CALNAME` and each VTIMEZONE once; objects' own
      calendar properties are not repeated.
    - **Navigation wraps on a phone** instead of scrolling sideways: with
      four sections, a scrolled-away link was hidden (decision 12).
31. **Deploy and hand-off (step 7, 2026-11-06).**
    - **The server handles SIGTERM** as well as Ctrl-C (Converge's
      pattern). As PID 1 in its container it otherwise ignored the stop and
      was killed after the grace period, on every `docker compose down` and
      every rolling backup.
    - **Backups are the privacybox rolling backup** of
      `$DOCKER_ROOT/calzone-server` (the app is stopped while its tree is
      archived, so the Postgres files are consistent); a `pg_dump` and the
      per-person export zip are the portable extras. No backup code in
      CalZone.
    - **Leaving Baikal (`docs/leaving-baikal.md`):** copy each account, re-run
      until everything is unchanged, share, then move every device on the
      same day. A re-run of the copy overwrites CalZone edits with Baikal's
      version and does not carry deletions, so copying stops once CalZone
      takes writes. Baikal stays untouched until retired.
    - **The restore drill** (exit criterion 5) uses the local image stack
      (`make up`) as the fresh CalZone.
32. **Two-factor sign-in for the web app (2026-11-06).** CalZone is public
    facing (a work machine cannot join NetBird), and a web session can make
    device passwords, so the web login is the door worth locking.
    - **TOTP** (RFC 6238: HMAC-SHA1, 6 digits, 30 s, one step of drift
      either way) from any authenticator app; each step is accepted once
      (an atomic `totp_last_step` update). Ten single-use recovery codes
      (`abcde-fghij`, 50 bits, SHA-256 stored, case and spaces ignored).
      Passkeys may come later, as an addition: a managed work laptop may
      block them.
    - **Secrets at rest:** AES-256-GCM under `CALZONE_SECRET_KEY` (32 bytes,
      base64), the principal id as associated data, so a secret copied to
      another row does not decrypt. `serve` refuses to start without the
      key. A lost key means everyone sets two-factor up again.
    - **Login:** the passphrase alone gives a pending session (10 minutes,
      `web_sessions.verified = false`) that reaches only `/login/code`; a
      right code ends it and starts a fresh full session (a new token).
      Wrong codes share the login budgets (10 per address, 5 per account
      per 15 minutes); when spent, the pending session is ended.
    - **Policy `CALZONE_TWO_FACTOR`:** `admins` (default), `everyone` (the
      deploy examples and privacybox, being public) or `optional`. Someone
      who must use it and has not set it up is sent to the setup page by
      every page (HTMX gets `HX-Redirect`); setup and logout stay
      reachable. Turning it on ends the person's other sessions. They can
      turn it off (passphrase plus code) only when not required.
    - **Recovery:** an admin's "Reset two-factor" on People or `user
      two-factor-reset NAME` turns it off and ends that person's sessions;
      they set it up again at the next login. Admins cannot reset their own.
    - **DAV app passwords are unchanged:** 125 random bits per device,
      revocable, and DAV clients cannot do a second factor.
    - **The setup QR code** is inline SVG (the `qrcode` crate), with fill
      attributes only, so the CSP allows it; dark on white in both themes.
33. **"Keep me signed in on this device" (2026-11-06).** Off by default, so
    a work or shared computer is the safe case. Off: the server ends the
    session after 12 hours and the cookie has no `Max-Age`, so the browser
    drops it when it closes. On: 30 days, as before. The choice is stored
    on the session (`web_sessions.persistent`): a pending two-factor
    session hands it to the session that follows, and a passphrase change
    keeps it.
34. **Refused sign-ins are logged for CrowdSec (2026-11-06).** One fixed
    line per refusal (`server::audit`): `auth_failure service=web|code|dav
    reason=wrong|throttled ip=<address>`, target `calzone::auth`, level
    WARN. Throttled refusals are logged too, so a guesser that keeps going
    past CalZone's own budget still fills CrowdSec's bucket. Never the name
    tried (people type passphrases into it) or any credential. The address
    is the throttling one (`CALZONE_BEHIND_PROXY`). Logs carry no colour
    codes unless stdout is a terminal. `deploy/crowdsec/` holds the
    acquisition, a parser for exactly this line (pinned by
    `audit::tests`) and a scenario: more than 10 refusals in about 10
    minutes from one address. The line format is a contract: change the
    parser with it.
26. **S2 result (2026-11-06): vkurko EventCalendar 5.18.1 for the grid.**
    Measured in headless Chromium under our exact CSP, week view with
    timed and all-day events, desktop and phone width:
    - **EventCalendar:** no CSP violations (it styles through the CSSOM,
      which `style-src 'self'` allows), events placed and sized right,
      drag fires `eventDrop` at both widths. 133 KB minified, 44 KB
      gzipped, no runtime dependencies in the build. Stable `ec-*` classes
      and 30 `--ec-*` custom properties, so it takes our tokens (decision
      13). MIT, confirmed from the repo's LICENSE (vkurko/calendar).
    - **FullCalendar 7.1.4:** also works under the CSP and drags, but 185
      KB gzipped, and its class names are hashed (`fc-classic-TZ4`), which
      makes theming with our tokens impractical. Not chosen.
    - **Our own component:** not needed now; it stays the fallback if
      EventCalendar stops being maintained (single maintainer). The build
      is vendored and pinned like HTMX: `static/event-calendar.min.js`
      sha256 `31318d0a1b55fbfd9a8ef2009ca760f24f181a9b0a68edcf1d4ae0f93813a810`,
      `static/event-calendar.min.css` sha256
      `d01c61b02280d891f14a4fd9dfbcd708678b6fab4245b8503ad6ec304e44584c`.
35. **English and Dutch (2026-11-06).** Every page and message exists in
    both; English is the default. Each person picks theirs in Settings;
    before login (and until they pick), the browser's `Accept-Language`
    decides. Quick add understands both (decision 12). Strings live in one
    place per language, never inline in handlers, so a page cannot ship
    half translated.
36. **Dark by default (2026-11-06).** The whole app, the grid included,
    starts dark; each person can choose light or "follow the device" in
    Settings. The grid is themed only through its `--ec-*` properties set
    from our tokens (decision 13), so it always matches the page around it.
37. **The calendar API (phase 2 step 2, 2026-11-06).**
    - **JSON under `/api/`** for the calendar page:
      - occurrences, events (create, update, delete with
        `instance` + `scope`), undo, quick add, settings, calendars and
        per-calendar prefs;
      - the session cookie and the Origin guard as for pages; `ApiUser`
        answers JSON 401/403 instead of redirecting;
      - errors are `{"error": ...}`: 412 stale, 409 undo conflict, 404
        nothing to undo.
    - **Times:** the browser speaks viewer-local wall time (`2026-01-05`
      or `2026-01-05T09:00`). The server keeps each event's own kind: a
      floating event stays floating, UTC stays UTC, and a zoned event keeps
      its zone, converted. New events take the viewer's zone, which is
      their setting, else `CALZONE_DEFAULT_TIMEZONE`, else UTC.
    - **Every web write is one transaction:** the lock, the `If-Match`
      check against the ETag the browser saw, the core edit on the
      viewer's own view (their reminders merged in), the store, and an
      `object_history` row with the replaced body and every person's
      layer.
    - **The sequence advances within a transaction** (`Locked.sequence`),
      so the two objects of a split get distinct change-log numbers.
    - **Undo:** by the author only, within 30 days, once. It restores
      bodies and every layer, and is refused when any touched object
      changed since.
    - **Splits keep other people's reminders:** their series-wide VALARMs
      are copied to the new series (the editor's travel in the text);
      reminders and snoozes for single occurrences stay with the old one.
    - **Settings:** time zone, language (`NULL` follows the browser),
      theme (`dark` by default), week start and 24-hour clock per person.
      Per calendar and person: colour, name, hidden, order and default
      reminder (applied to events that person creates).
38. **The calendar page (phase 2 step 3, 2026-11-06).**
    - **`/calendar` is home** (also after login and the code step). Our own
      translated toolbar is wrapped around the EventCalendar grid:
      - today, back and forward, and the title;
      - agenda, day, week and month (phone default agenda, desk default
        week);
      - quick add, and "+".
      The view and date live in the URL.
    - **`static/calendar.js`** feeds the grid from `/api/occurrences`:
      - a drag shows at once and snaps back if refused; a recurring event
        first asks the three-way scope in a `<dialog>`;
      - every write shows a toast with Undo for 10 seconds (no confirm
        dialogs);
      - keys `t`, `n`, arrows and `j`/`k`, `1`-`4`, `Esc`; swipe pages on
        touch;
      - on first use it sends the browser's time zone.
      Titles are inserted as text, never HTML.
    - **The event panel** is an HTML fragment (`/calendar/event`,
      `/calendar/event/new`) shown as a side sheet, full screen on phones.
      The script sends only the fields that changed. All-day events show
      their last day and store the day after. Reminders are the viewer's
      own.
    - **"All of them" on one occurrence** treats a new start or end as a
      move of that occurrence, so the series shifts by the same amount
      (core `edit::rebase`); it never jumps to the occurrence's date.
    - **Scripts load on every page:** `hx-boost` swaps only the body, so a
      script in one page's head would never run after a boosted
      navigation. The calendar starts on load or `htmx:afterSettle`, and
      its document listeners act only on the live page.
    - **Grid theming:** EventCalendar sets `color-scheme: light` on `.ec`,
      which flips `light-dark()` tokens. Our unlayered `.cal .ec` rules set
      `color-scheme: inherit`, map its `--ec-*` and grey scale to our
      tokens, and set its polyfill pair from `data-theme`.
    - **Translations:** `web::i18n::T`, one struct per language, also sent
      to the script as JSON. Translated now: base, navigation, login, the
      code step, the calendar page and panel. The older admin pages follow
      in step 5.
    - **Owners show as initials** of their display name on events in
      calendars the viewer does not own.
39. **Contacts (phase 2 step 4, 2026-11-06).**
    - **Core `card`:** reads a vCard 3.0 or 4.0 into name, organisation,
      phones, emails, addresses, birthday, note and photo, and applies a
      change losslessly:
      - only touched lines are rewritten; `REV` changes only when
        something did;
      - repeating fields keep every line whose type and value are still
        wanted, byte for byte, and add new ones next to their kind;
      - unknown properties, groups and photos stay;
      - a field counts as changed only when the edit supplies a different
        value (a stray space in a stored name is not a change);
      - the shown name (FN) follows the names only when it was derived
        from them; a contact is refused only when no shown name would
        remain (cards with only a shown name stay editable);
      - a birthday without a year is never lost by a save that leaves it
        alone.
      New cards are vCard 3.0, the version the door advertises.
    - **Pages:** `/contacts` (live search through `hx-get`, letters, an
      avatar or initials), one contact (tel:, mailto:, an OpenStreetMap
      search link that opens outside), and the edit form.
      - Plain forms with post-redirect-get; the parallel row fields carry
        two spare phone rows and one spare email and address row, so no
        script is needed.
      - Saves and deletes go through the event history (conditional on the
        ETag, undoable): a notice with Undo, never a confirm dialog. A stale
        form lands on the latest version with a notice.
    - **Emails are a text field with the email keyboard**, not `type=email`:
      browsers reject addresses with non-ASCII letters, which would block
      saving the whole contact.
    - **Photos:** inline JPEG, PNG, GIF and WebP are served from
      `/contacts/photo/...`; links in a card are never fetched.

## Roadmap

| Phase | Builds | Replaces | Done when |
|---|---|---|---|
| 1 | core, storage, domain service, CalDAV/CardDAV door, admin web pages, Baikal import, export | Baikal | DAVx5 + Fossify and Thunderbird sync on both phones with no duplicates or lost edits |
| 2 | sharing UI, per-user layer UI, the calendar and contacts web app, ICS publishing and subscriptions | (new) | two people share a calendar, and the web app passes decision 12 |
| 3 | MCP door, PAT scopes in the UI | external caldav-mcp | Hermes reads and writes within its scope only |
| 4a | `crates/ffi`, sync API, Android store + system bridge | DAVx5 | Fossify works on CalZone's bridge |
| 4b | Android calendar UI, reminders, push | Fossify | daily use for a few weeks |
| 5 | invitations, free/busy, birthdays calendar from contacts | | |

## Invariants

- **Round trips are lossless:** parse then serialize keeps every property,
  parameter and component of the corpus (fixtures plus the household's real
  export). Line folding and property order may change; content may not.
- **One change log:** every write and delete bumps the collection sequence
  in the same transaction as the write.
- **Doors never touch the database;** they call the domain service.
- **Recurrence is the core's alone;** no other crate or client expands RRULE.
- **Projection bounds contain every occurrence** (floating and all-day
  widened by 14 hours), checked over the whole corpus.
- **Quoted parameter values are one value,** commas included (RFC 5545 3.2).
- **Alarms are per user:** a stored shared object holds no VALARM, and no
  user is ever served another user's alarms.
- **People only reach collections they own or were granted;** anything else
  is `404`, never `403`, so names do not leak. A feed token reaches exactly
  one calendar at its detail level, and a revoked or unknown token is `404`.
- **Subscribed feeds keep the last good copy:** a failed, partial or empty
  refresh never replaces stored events.
- **Conditional writes are honoured:** `If-Match` / `If-None-Match` are
  checked under a row lock; a stale write is refused, never merged silently
  by the DAV door.
- Strict CSP, no inline script or style; browser writes need a same-origin
  `Origin` and `SameSite=Strict` cookies.
- Logs never contain event or contact content, tokens, passwords or push
  URLs.
- **`fixtures/private/` is confidential household data** (the Baikal
  export). It is gitignored, mode `700`/`600`, never committed, copied,
  uploaded or quoted. Code that reads it prints only counts and strict
  property-name tokens (`[A-Z0-9.-]{1,40}` followed by `;` or `:`); anything
  else is reported as `<non-property line>`. Learned 2026-11-06: a looser
  name extractor printed two fragments of an event description when a
  candidate library split lines. Tests that need the private corpus print
  `SKIP` when it is absent.
- A new crate needs a `COPY crates/<name>/Cargo.toml` line in
  `docker/Dockerfile`; the release tag must equal `[workspace.package]
  version`.
- Anchored edits by tools fail loudly: assert the match count.
- The owner does all git; never commit, tag or push.
