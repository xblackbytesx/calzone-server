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
dashes. The phase 1 plan is `docs/plan-phase-1.md`; phase 2 is `docs/plan-phase-2.md`; phase 3 (drafted, not started) is `docs/plan-phase-3.md`; phase 4 (under way, before phase 3) is `docs/plan-phase-4.md`.

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
workspace tests. Step 5 done the same day: reminders on view-only
calendars, the calendar list with sharing, preferences, every page in
Dutch and English, phone navigation (decision 40). 175 workspace tests.
Step 6 done the same day: ICS feeds both ways (decision 41), migration
`0009_feeds.sql`. 185 workspace tests. Step 7 under way: the
accessibility and layout pass is done (decision 42; axe-core finds
nothing on any page), README screenshots from a made-up household. Phase 2 is closed
(2026-11-07); its household test (decision 12) moved to 4b, on the app.
Phase 4a: spikes S4 (decision 44) and S5 (decision 45) decided
2026-11-07 and 2026-11-09. Step 1 done 2026-11-09: the core's sync types
and three-way merge (decision 46), checked on every household event and
card. Step 2 done 2026-11-09: the sync API, phone sign-in by QR code and
UnifiedPush (decision 47, migration `0010_sync.sql`), 216 workspace tests.
Step 3 done 2026-11-09: `crates/client` (the phone's store and sync
engine) and `crates/ffi` (decision 48), `calzone-server resync` for
restores (migration `0011_sync_floor.sql`), 227 workspace tests. Step 4
done 2026-11-09: the app in `/workspace/calzone` (sign-in, account, sync,
push, settings; its own `AGENTS.md` holds the app's decisions),
device-untested. Step 5 done 2026-11-09: the calendar bridge (decision 49),
238 workspace tests; device-untested. Step 6 done 2026-11-09: the
contacts bridge (decision 50), 247 workspace tests; device-untested. Step 7
built 2026-11-09: 0.6.0 (the core the first app release pins), the app's
release workflow and its `docs/switch-over.md`; the switch itself, on both
phones, and a few weeks of daily use are the owner's, and close 4a.
Phase 4b planned 2026-11-10 (`docs/plan-phase-4b.md`, a draft for the
owner): spike S6 (reminders) first, then steps 1 to 7. S6 decided
2026-11-11 (decision 51). Step 1 done 2026-11-11 (decision 52): the
core's `view` shared by the web and the phone, repeats with an end on
the web, `calzone_client::calendar` and its FFI, `PUT
/sync/v1/collections/{id}/mine`, and recur's fix 5 (a UTC `UNTIL` of a
zoned series kept its last occurrence); 258 workspace tests. Step 2
done 2026-11-11: the agenda as the app's main screen (app decisions 23
to 26), device-untested. Step 3 done 2026-11-11: day, week and month
views (app decisions 27 to 29), device-untested. Step 4 done 2026-11-11:
editing, quick add, the scope question, undo, and my layer per calendar
(app decisions 30 and 31), device-untested. Step 5 done 2026-11-11:
the app rings its own reminders (app decision 32) from the core's new
`calendar::reminders` (each occurrence with its own part's VALARMs, start,
end or fixed triggers; FFI `reminders`), device-untested. Step 6 done
2026-11-11: search (`calendar::search`, FFI `search`), "add to calendar"
and opening events from other apps, and two Glance widgets (app
decisions 34 and 35), device-untested. 0.7.0 prepared 2026-11-11 for
the owner's tag (the core of 4b steps 1, 5 and 6, which the app's v0.2.0
pins; no new migrations). Next: step 7, the household test and the
release.

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

## Crates

| Crate | Role | Rule |
|---|---|---|
| `crates/core` (`calzone-core`) | domain model, iCalendar/vCard parse and serialize, recurrence expansion, per-user layer split, sync types, three-way merge | Pure: no IO, no clock (time is passed in), no randomness |
| `crates/dav` (`calzone-dav`) | CalDAV/CardDAV/WebDAV paths, request bodies, query filters, multistatus XML | Pure like the core: no IO; the server's `dav` module does auth and calls the domain (decision 28) |
| `crates/server` (`calzone-server`) | axum + sqlx Postgres + askama/HTMX: domain service, storage, auth, web app, sync API, MCP, push | Runtime sqlx queries only (no `query!`), so builds never need a database |
| `crates/client` (`calzone-client`) | the phone's store (SQLite), pending changes and sync engine | Talks to the server only through the sync API |
| `crates/ffi` (`calzone-ffi`) | UniFFI bindings for the app, over `calzone-client` | Thin: JSON in and out (the core's own shapes), `flat_error` for real errors |

Server modules: `config`, `cli` (admin commands), `client` (client
address), `audit` (the `auth_failure` line), `throttle` (login failure budgets), `domain` (`access`,
`principals`, `collections`, `objects`: put, get, get_many, list, in_range,
delete, changes_since; `app_passwords`: create, list, revoke, verify;
`accounts`: passphrases, admins, sessions; `imports`: reports;
`two_factor`: TOTP, recovery codes, secrets at rest; `prefs`: settings;
`events`: occurrences, writes, undo (on `calzone_core::view`); `contacts`: list, detail, writes,
photos; `feeds`: secret links and the published feed; `subscriptions`:
subscribed calendars, schedule, `replace`; `devices`: sign-in codes, device
tokens, push endpoints; `sync`: the collection list, change batches,
merged uploads), `feeds` (the SSRF-guarded
fetcher and the refresher task), `sync` (the `/sync/v1` door), `push`
(the change listener and UnifiedPush sends), `webpush` (RFC 8291), `import`
(`Run`: files and Takeout zips; `baikal`: the CalDAV/CardDAV reader;
`digest`), `export` (zip, one collection), `dav` (`auth`: Basic; `props`:
live properties; handlers), `web` (`session`, `pages`, `collections`,
`passwords`, `admin`, `transfer`, `two_factor`, `api` (JSON for the
calendar page), `calendar` (the page and the event panel), `contacts`, `feeds`
(`/feeds/<token>.ics` and the link and subscription endpoints),
`i18n`
(languages and every string); askama templates in `templates/`, assets in
`static/`), `migrations/` (numbered SQL, embedded, run at every start).

Client modules: `store` (Store: collections, objects, pending, conflicts,
local changes), `engine` (sign_in, sync, sign_out, push registration),
`transport` (the app's HTTP), `link` (the sign-in address), `bridge`
(Android's calendar and contact rows and edits), `calendar` (the app's
own screens: occurrences with an Expansions cache, event, create, update,
remove, undo, quick_add, set_mine).

Dav modules: `xml` (namespaced tree, escaping), `path` (URL space, hrefs),
`request` (PROPFIND, PROPPATCH, MKCOL/MKCALENDAR, REPORT bodies), `filter`
(calendar-query and addressbook-query), `response` (multistatus, errors).

Core modules: `android` (rows, apply, new_event: Android's calendar), `android_contacts` (row, apply, new_contact: Android's contacts), `lines` (Document, Component, Item, Line; fold), `object`
(inspect_calendar, inspect_card, Bounds, unescape), `recur` (expand, When,
Occurrence, Warning), `vtimezone` (VZone), `split` (split, merge,
UserLayer), `import` (decode, split_calendar, split_cards, assemble),
`moment` (Moment, Key, VTIMEZONE generation), `edit` (EventChange, Scope,
edit, delete, new_event), `view` (occurrences, detail, change_for,
EventInput, Repeat, quick_draft, initials: the calendar as a person sees
it, for the web and the phone), `quickadd` (parse, Lang), `card` (read, apply,
new_card), `feed` (object_for_feed, publish: full or busy-only). Per-user items: VALARM, `X-MOZ-LASTACK`, `X-MOZ-SNOOZE-TIME`
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
| `make android-core` | `calzone-ffi` for Android (arm64, x86_64), its Kotlin bindings and a host library, into `dist/` |

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
   see, each user keeps their own color, display name, visibility and
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
    - Assistants (Hermes) get their own account and tokens; collections
      grant them access one by one (decision 43, which replaced the
      personal access tokens first planned here).
    - DAV clients use app passwords, scoped like PATs, only while the DAV
      door is on.
    - Every token and password is revocable on its own. Only hashes are
      stored. Login answers the same for every failure and always pays one
      Argon2 verification.
11. **Web app: HTMX shell plus one grid island.** Pages, forms, the event
    panel, contacts, settings and sharing are HTMX (`hx-boost`, View
    Transitions, preloading neighboring ranges). The day/week/month time
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
    who owns an event is visible through color and an avatar; undo after
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
      (the license allows it) and never take code from the AGPL successor.
    - **Our wrapper corrects four calcard behaviors, each pinned by a
      test (`crates/core/src/recur.rs`):**
      - Occurrences of a UTC (`Z`) recurring event come back labeled
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
      rename, recolor, share and delete). The owner cannot be granted.
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
      stored as SHA-256 of the normalized form (case, spaces and dashes
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
    - **Properties:** PROPPATCH sets display name, description and color
      only, all or nothing (403 plus 424). MKCALENDAR also takes the
      component set, and accepts and drops `calendar-timezone(-id)` and
      `calendar-order` (DAVx5 sends them; objects carry their own zones).
      Address books advertise vCard 3.0 only: cards are stored as sent.
    - **Queries:** component, time range and property filters
      (defined, not defined, text match). Anything else is `403
      supported-filter`. A time range uses the projection, then expands a
      recurring event in a closed range, widened by the floating margin.
      `expand` in `calendar-data` is not honored: objects are served
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
      describe, recolor, share with a person or group as view, edit, or
      edit and share; delete only with the name typed). Devices (app
      passwords, scoped per collection, shown once with the server address
      and user name; hidden when the DAV door is off). People (admins:
      add, new passphrase, admin on or off, disable or enable; groups and
      their members, users only). Settings (display name, passphrase).
    - **Security:** the Converge guard: a cross-site `Origin` on any write
      is refused, CSP `default-src 'none'` with `self` scripts and styles,
      `nosniff`, `same-origin` referrers. No inline script or style; a
      test reads every template. Calendar colors are SVG `fill`
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
      missing, with its name, description, color and components);
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
    is the throttling one (`CALZONE_BEHIND_PROXY`). Logs carry no color
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
      Per calendar and person: color, name, hidden, order and default
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
      `color-scheme: inherit`, map its `--ec-*` and gray scale to our
      tokens, and set its polyfill pair from `data-theme`.
    - **Translations:** `web::i18n::T`, one struct per language, also sent
      to the script as JSON. Translated at first: base, navigation, login,
      the code step, the calendar page and panel; everything else in step 5
      (decision 40).
    - **Owners show as initials** of their display name on events in
      calendars the viewer does not own.
39. **Contacts (phase 2 step 4, 2026-11-06).**
    - **Core `card`:** reads a vCard 3.0 or 4.0 into name, organization,
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
40. **My layer, sharing and every page in two languages (phase 2 step 5,
    2026-11-06).**
    - **Reminders on calendars you can only view:** a reminder-only change
      goes through `events::update_reminders`:
      - the core's `edit::set_alarms` touches only VALARMs, no stamps;
      - the server stores it only if the shared object comes out byte for
        byte the same, so it is allowed with view access, covers every
        occurrence, and can be undone;
      - undo without write access is allowed exactly when it restores
        layers only.
      The panel keeps the reminder menu outside the disabled fields and
      skips the scope question for reminder-only saves.
    - **The calendar list:** beside the grid on a desk (folded away with
      the Calendars button, remembered per browser), above it on a phone.
      - A checkbox shows or hides a calendar.
      - "⋯" opens the settings sheet: my name, my color (stored only when
        actually picked, so you otherwise follow the calendar's own) and my
        default reminder.
      - When you may share it, the sheet also holds sharing, through the
        existing share endpoints.
    - **Settings has Preferences:** language (or the browser's), theme,
      time zone (every IANA zone), first day of the week, 12- or 24-hour
      clock.
    - **Every page is translated (decision 35):**
      - the admin pages' strings are `i18n::A` (`nav.a`), generated with
        `T` from one list, so both languages always have every field;
      - handler messages use the person's table;
      - generic domain errors (not found, read-only, name taken, bad name,
        server, admin only) carry a key that the guard translates in the
        reader's language (their setting, else the browser's), and the
        plain error page is translated too;
      - a test renders every page for a Dutch user and fails on common
        English words.
    - **Phone navigation:** Calendar and Contacts in the bar, the rest
      behind "More". "Calendars & contacts" is now "Calendars & address
      books".

41. **ICS feeds (phase 2 step 6, 2026-11-06),** decision 24 built.
    - **Links:** whoever may share a calendar makes links from its
      settings sheet, with a label and a detail level. The token is 256
      random bits (base64url), shown once with an `https://` and a
      `webcal://` copy button; only its SHA-256 is stored
      (`feed_links`). The sheet lists label, detail, made and last used;
      removing a link is immediate. A disabled owner's feeds stop.
    - **The feed** (`GET /feeds/<token>.ics`, no session, DAV door
      independent): events only, from shared bodies, so nobody's
      reminders. Busy-only keeps identity, times, recurrence and status,
      titles every entry "Busy" (in the language of whoever made the link,
      since feed readers send none) and leaves out free and canceled
      events; overrides always stay, or the original occurrence would come
      back. A full feed publishes `CLASS:PRIVATE` or `CONFIDENTIAL` events
      busy-only. ETag `"<sequence>-<detail>"`, `Last-Modified`, 304,
      `noindex`.
    - **Subscribing:** "+ Subscribe to a calendar" under the calendar list
      takes an address (`webcal://` means https), an optional name (else
      the feed's `X-WR-CALNAME`, else the host) and an interval (15 min,
      hour, 6 hours, day). The address is fetched before anything is made,
      and a copy that does not read completely makes no calendar.
    - **Read-only:** a subscribed calendar is a normal calendar with a
      `subscriptions` row; the object lock downgrades everyone to read
      (DAV privileges, the API and the panel follow). Its owner may still
      rename, recolor, share and delete it, and everyone keeps their own
      reminders on it.
    - **Refreshes** (a task every minute, ten due at a time, or "Refresh
      now") send `If-None-Match` / `If-Modified-Since`, and replace the
      copy in one transaction only when every entry was accepted by the
      store and there is at least one: new and changed UIDs are stored
      (named after a hash of the UID), missing ones deleted, unchanged ones
      left alone. Refreshes store the shared part only, so people's own
      reminders survive (the feed's alarms are dropped). Failures keep the
      copy, show on the sheet in the reader's language, and back off:
      double the interval per failure in a row, at most a day.
    - **The guard** (`server::feeds`): resolve once and check every
      address (no loopback, private, link-local, CGNAT, multicast,
      unspecified, NAT64 or v4-mapped private addresses), pin the
      connection to the checked address, follow at most 3 redirects by
      hand and check each, 10 s to connect, 30 s and 10 MiB in all, no
      user name or password in the address. `CALZONE_FEEDS_ALLOW_PRIVATE=on`
      lifts the address check for a feed on the LAN (default off).
    - **Errors are fixed English sentences** without content (stored as
      `last_error`), translated when shown. A failed refresh answers 422,
      not 502, so the access log does not report an outside feed's
      trouble as a server error.

42. **Accessibility and polish (phase 2 step 7, 2026-11-06).**
    - **The check:** axe-core (WCAG 2.0/2.1 A and AA) over every page,
      the four calendar views, both panels and both sheets, at phone and
      desk width, light and dark, English and Dutch, against the real
      binary with a made-up household; plus console, CSP and overflow
      checks and a keyboard walk. Result: no findings. Rerun it after UI
      changes; the household's real data never goes into screenshots.
    - **Event colors:** text on an event is pure black or white,
      whichever contrasts more with the calendar color. With pure black
      and white every color reaches 4.5:1 (the worst case is about 4.58);
      with near-black some cannot. The time is not faded, and the owner's
      initials are a ring, not a tinted fill.
    - **The grid for screen readers:** EventCalendar labels its layout as
      a table but puts navigation and event buttons straight into rows.
      `calendar.js` removes those layout roles and index attributes (a
      MutationObserver keeps them off) and gives every event an
      `aria-label`: title, day, time or "all day", place, calendar and
      whose. The agenda view is the list view.
    - **Short events** (under 40 minutes) are one line with an ellipsis;
      in the time grid the title comes first (the block's place already
      shows the time), and a phone's week view leaves the time out.
      Initials are first and last word everywhere ("Bram de Vries" is BV).
    - **Elsewhere:** links inside sentences are underlined (WCAG 1.4.1);
      a "Skip to content" link; toasts are filled after they appear so
      screen readers announce them; the light theme's green is darker
      (`#176a3f`) for 4.5:1 on its tint.
43. **Assistants and the MCP door (phase 3 design, 2026-11-07),**
    decisions 10 and 17 made concrete; the plan is `docs/plan-phase-3.md`.
    - **One assistant for the household,** not personal tokens: Hermes
      answers both parents in a shared Matrix room and in DMs. An
      assistant is a third kind of principal, named by an admin; no
      passphrase, no web login, only `czt_` bearer tokens (purpose `mcp`,
      so DAV passwords and MCP tokens never open each other's door).
    - **Access is a grant per collection:** "MCP access" on the collection
      page (Off, Busy only, Read, Read and add, Read, add and change; Off
      by default), set only by
      whoever may share it. The existing sharing checks are the boundary.
      The `busy` and `add` levels are for assistants only: busy gives
      times and nothing else (stripped in the domain), add creates and
      undoes its own new objects.
    - **Assume the answers leave the house:** Hermes delegates to a cloud
      model. Every answer is small (31 days and 200 occurrences, 20
      contacts per call, a search text required, no export); contacts
      come through without street addresses, notes or photos.
    - **No deletes through the door** (trust first), enforced in the
      domain for assistants at every level; removing an occurrence or
      cutting a series short counts as one. The only exception: undoing
      its own new object. Every change records the acting principal;
      anyone with write access may undo an assistant's change.
    - **An assistant is nobody's account:** grants of read, add or write
      only (never admin), never a group member or admin, no passphrase,
      web login or app passwords; removing one disables it so the trail
      survives. At most 60 changes an hour. `CLASS:PRIVATE` and
      `CONFIDENTIAL` events come through as busy. The door takes fields,
      never raw iCalendar or vCard. Its new events get each person's own
      default reminder.
    - **Parked:** tasks (VTODO) in the door and the web app; reminders
      through the door (it cannot tell whose phone should ring).
    - The door is `/mcp`, Streamable HTTP, off unless `CALZONE_MCP=on`;
      the library is spike S3.
44. **S4 result (phase 4, 2026-11-07): the phone's store is Rust and
    SQLite, through UniFFI.** Measured with a throwaway crate, then
    deleted.
    - **rusqlite `0.32` with bundled SQLite.** It is the series on
      `libsqlite3-sys 0.30`, which sqlx 0.8 keeps in the graph even unused
      (Cargo checks `links` across optional dependencies). Both move
      together when sqlx 0.9 (rustc 1.94) comes.
    - **Shape:** the full text plus a projection (kind, UID, summary, time
      bounds as UTC seconds) with a time index; a range comes from the
      core's expansion (decision 9), contacts by name from the index.
    - **Timings (desk machine, release):** the household's size (185
      events, 1,412 contacts) fills in 74 ms, a month takes 7 ms, a week
      5.5 ms, an edit with its pending change 5 ms; 0.6 MB on disk. Ten
      times that: a month 77 ms, open-ended weekly series doing most of the
      work. A phone is a few times slower, so the household's size is
      fine; step 4 measures on the phones, and if a month passes 100 ms
      there, 4b adds an occurrence cache refreshed on change.
    - **Kotlin:** UniFFI 0.32 bindings (an object, `flat_error`) generated
      from the unstripped debug library, compiled with kotlinc and run on
      the JVM through JNA: writes, a recurring read, a typed refusal.
    - **Android:** NDK r27d with cargo-ndk. The arm64 library is 4.97 MB
      stripped (1.65 MB compressed in the APK; SQLite, calcard and the
      time-zone database), x86_64 5.1 MB. It needs only libc, libm and
      libdl, so no `libc++_shared.so` ships.
    - **minSdk 35, target and compile 37** (kagami's). Both phones run
      GrapheneOS on the newest Android; nothing older is supported.
45. **S5 result (phase 4, 2026-11-09): the system bridge works on
    GrapheneOS, and Android's own sync triggers start our sync.** Checked
    with a throwaway APK on a Pixel (Android 17, main profile), then
    deleted.
    - **Account:** a CalZone account type with a calendar and a contacts
      sync adapter, no Play Services.
    - **What Fossify writes,** read back as changes:
      - an edited event: a dirty row; reminders are their own rows;
      - one occurrence changed: a new exception row (ORIGINAL_ID and
        ORIGINAL_INSTANCE_TIME); the series row is untouched;
      - one occurrence deleted: an exception row at the original time
        with STATUS_CANCELED and no title, which the core turns into an
        EXDATE;
      - a series edit rewrites RRULE and DURATION in an equal form
        (`INTERVAL=1;BYDAY=MO`, `P0DT1H0M0S`), so the bridge compares
        by meaning, never by text;
      - a new event may end when it starts.
    - **Display:** exceptions, EXDATE, read-only access and calendar
      colors show correctly in Fossify.
    - **Writes to Events** select by `calendar_id IN (ours)`. Account
      columns exist only in the read view, so filtering on them fails.
    - **Contacts:** the account declares an EditSchema (DAVx5's list,
      GPL-3.0, credited). Without one the Contacts app treats the account
      as read-only: it saves edits as a new contact in another account
      and joins the two, and crashes when ours is chosen. Photos are
      written the DAVx5 way (the display-photo file, wait for the
      provider, clear the dirty mark it sets). Edits come back as dirty
      rows, deletes as DELETED; the dialer shows the names.
    - **Triggers:** Android runs our adapter with `upload` within seconds
      of an edit in Fossify or Contacts, on Fossify's refresh, on our own
      request, and daily. It merges repeated requests, so a sync that
      runs twice must do no harm. The adapter only starts the engine;
      push and the schedule start it too (step 4).
    - **Network:** Android starts a sync only when the app can reach the
      network. With GrapheneOS's Network permission off it holds every
      sync silently, and an update that adds the permission does not ask
      for it. So the app has internet from its first version, and its
      settings say plainly when Network is off.
46. **The three-way merge (phase 4 step 1, 2026-11-09): `merge` and
    `sync` in the core.** `merge(base, mine, theirs)` takes three texts
    (`None` is absent or deleted) and gives the merged text and what the
    phone lost.
    - **Units:** a component with a UID goes by UID and RECURRENCE-ID, so
      each occurrence of a series merges on its own; a VTIMEZONE by TZID;
      anything else (VALARM, a time zone's rules, unknown components) by
      its text, so an edit there is a removal plus an addition and both
      sides' versions survive.
    - **Fields:** a field only one side changed takes that side's value.
      Single-valued properties (a fixed list per iCalendar and vCard)
      changed on both sides keep the server's lines and report the
      phone's. DTSTART, DTEND, DURATION and DUE are one field, `TIME`, so
      a start never pairs with the other side's end. Everything else,
      unknown properties included, merges as a set of lines: both sides'
      additions and removals apply. Lines are compared by their unfolded
      text, and kept lines keep their source bytes.
    - **Bookkeeping** (DTSTAMP, LAST-MODIFIED, CREATED, PRODID, REV,
      SEQUENCE) is never a conflict: SEQUENCE takes the higher number, the
      rest the server's line.
    - **Deletes:** deleted on one side and unchanged on the other is
      deleted. The phone deleting what the server changed keeps the
      server's copy (reported as a lost delete); the server deleting what
      the phone changed stays deleted (reported with the phone's text).
      Server wins, as decision 15 says; the web's undo history is the way
      back.
    - **Layout:** the server's order; the phone's new properties go after
      the server's, its new components at the end.
    - **`sync`:** `State` (a collection's change-log position), `Change`
      (name, revision, body or tombstone), `Batch` (since, state, changes,
      more) and `Push` (name, base, body) with `Pushed` (revision, merged
      body, conflicts), all serde, for step 2's JSON.
    - **Checked** on the household corpus with the core's own edits on
      both sides (185 events, 36 occurrence merges, 1,412 cards): nothing
      else lost, no occurrence moved. Known limit: vCard groups
      (`item1.TEL` with `item1.X-ABLABEL`) merge line by line, so two
      sides adding the same new group name could pair a label with the
      wrong number.

47. **The sync API (phase 4 step 2, 2026-11-09): `/sync/v1`, phone
    sign-in and push.** Built on decisions 10, 15, 16 and 46.
    - **Sign-in:** the Devices page shows a QR code holding
      `<public URL>/link/czl_<code>`, and the same address as text to
      paste: 256 bits, stored as SHA-256, one use, 10 minutes. It comes
      from a signed-in session, so no second factor is asked again. In a
      browser that address is only a page saying to use the app. The app
      posts the code and its own name to `POST /sync/v1/link` and gets a
      device token `czd_...` (shown once, hash stored, last use recorded at
      most once a minute). A phone reaches everything its person can;
      scoped app passwords stay for other apps.
    - **Devices page:** always shown now. Phones are listed with "push on"
      and removed one by one (token and push endpoint go at once). The app
      password half shows only while the DAV door is on.
    - **Routes:** `POST /link`; `GET`/`DELETE /me` (the account; sign this
      phone out); `PUT`/`DELETE /push`; `GET /collections`;
      `GET /collections/{id}/changes?since=&limit=`; `POST
      /collections/{id}/changes`. Bearer token; a disabled person or a
      removed phone gets `401`. Mounted whatever `CALZONE_DAV` says.
    - **JSON:** the core's `sync` types, snake_case. Errors are
      `{"error", "code"}`, and the app branches on the code only:
      `unauthorized`, `not_found` (unseen looks the same), `bad_state`,
      `bad_request`, `link_used`, `push_off`, `bad_endpoint`, `too_large`,
      `server`.
    - **Changes:** each object once at its newest revision, oldest first,
      with the person's layer. A batch holds at most `limit` changes
      (default 200, at most 1000) and about 4 MB of text; cut short, its
      state is its newest revision, so asking again from there misses
      nothing. No `since` is a full sync whose first batch has no
      tombstones; later batches are "changes since" and may carry a
      tombstone for something the phone never had, which it ignores. A
      state ahead of the collection is `409 bad_state`: the phone syncs
      from the start. One repeatable-read snapshot per batch.
    - **Uploads:** up to 500 edits to one collection, in one transaction
      under the collection lock. Each is merged with the person's current
      copy (decision 46). An edit that changes nothing writes nothing (no
      new revision, so phones never ping-pong). With read access only the
      person's own reminders may change; the shared part must come out
      byte for byte the same, as on the web. A refused edit is listed
      with its reason (`read_only`, `invalid`, `uid_taken`, `bad_name`)
      and the rest go through. The answer is not a state: the phone pulls
      afterwards. Sync writes are not in the web's undo history.
    - **Push:** Postgres triggers (migration `0010_sync.sql`) send
      `NOTIFY calzone_changes` at commit for a collection's new sequence,
      name, description or color, a new or changed grant, and (to that
      person only) their own calendar settings. So every door, the CLI,
      imports and feed refreshes wake phones without knowing about push.
      One task listens, gathers for 2 seconds, and sends each phone whose
      person can see a changed collection one encrypted message:
      `{"collections": [{"id", "state"}], "all"}` (`all` past 40
      collections). Keys are required; nothing goes out in plain text.
      `CALZONE_PUSH_HOSTS` empty means push is off. A removed grant is not
      pushed (that person lost access); their phone drops the collection
      at its next sync. Push is a hint: a notice lost while the listener
      reconnects costs freshness, never data.
    - **Golden files:** `crates/server/tests/golden/*.json`, written by
      `tests/golden_pg.rs` (`UPDATE_GOLDEN=1` regenerates them on
      purpose); the app's contract test reads them.
    - **Checked** on Postgres (`tests/sync_pg.rs`): sign-in with the DAV
      door off, codes that work once and expire, disabled people, removed
      phones; the collection list with each person's view, paging,
      tombstones, a bad state; two phones offline with a web edit in
      between (different fields both survive, the same field keeps the
      server's value and reports the phone's, both phones end on one
      copy, nothing duplicated, sending it again writes nothing); a
      reminder on a read-only calendar stays personal; pushes to a
      stand-in push server, decrypted as the phone would: the right
      phones, one message per gather, a person's own settings to that
      person only, and a `410` drops the endpoint.

48. **The phone's store and sync engine (phase 4 step 3, 2026-11-09):
    `calzone-client` and `calzone-ffi`.** Built on decisions 14, 44, 46
    and 47.
    - **HTTP is the app's.** The engine says what to send through a
      `Transport` trait; the app implements it with OkHttp (through
      UniFFI's `HttpTransport`, a foreign trait), adding the server's
      address and the device token. TLS, the phone's own certificates and
      GrapheneOS's Network permission stay Android's, and the library
      carries no TLS. "No answer" and "an answer" are distinct, so the
      engine tells "try later" (`Network`) from "sign in again"
      (`SignedOut`, on any `401`) and from an unexpected answer (`Server`).
    - **The store** (SQLite, decision 44's shape): collections as the
      server lists them with the state this phone applied; objects as full
      text plus a projection (kind, UID, summary, UTC bounds) and a
      candidates-in-range query; pending edits, each with the copy it
      started from and a version; conflicts to show; and a local change
      log of what came from the server, which the bridge reads and then
      forgets (step 5).
    - **Edits** show in the store at once and wait as pending. A second
      edit keeps the first one's base; made and deleted before a sync, it
      never reaches the server. The text must parse and suit the
      collection; whether the person may make the edit is the server's
      call.
    - **One run:** list the collections (gone ones go, with their waiting
      edits kept as conflicts), upload pending edits per collection in
      chunks (200 edits or about 4 MB), then pull every collection whose
      state moved. Edits go up before changes come down, so the server
      merges against its newest copy. A run with nothing to do is one
      request; a second run in a row does no harm (decision 45).
    - **Answers:** an edit still as sent is done and the merged copy
      replaces it. An edit changed again while its answer was on the way
      is rebased onto the merged copy with the core's merge and waits for
      the next run, so no answer overwrites a newer edit. A refused edit
      goes back to the copy it started from and is kept as a conflict
      with the phone's text. Merge conflicts are kept too, for the
      person.
    - **Pulls:** each batch is one transaction with its state, so a run
      cut off keeps what it applied. An object with an edit waiting is
      left alone (the edit goes up next run). A full sync (the first, or
      after `409 bad_state`) stores its state only when done and then
      drops what the server did not list.
    - **Signing in** from the QR code's address (`link::parse`). Signing
      in again to the same account on the same server keeps the store,
      waiting edits included; anything else starts empty. Signing out
      wipes it.
    - **After restoring a backup:** the server's change log goes back in
      time and later writes could reuse numbers a phone already saw, so it
      would miss changes without knowing. `calzone-server resync`
      (migration `0011_sync_floor.sql`) renumbers every collection's log,
      revisions and undo history past a gap and refuses older states, so
      every phone and DAV client syncs from the start. It is part of the
      restore procedure in `deploy/README.md`.
    - **`calzone-ffi`:** one `CalZone` object over the store, JSON in and
      out (the core's and the client's serde shapes), a flat error per
      kind (`CzException.Network`, `.SignedOut`, `.Server`, `.Input`,
      `.Store`), and `parseLink`. Calls block; the app makes them off the
      main thread. `make android-core` builds arm64 and x86_64 libraries
      (minSdk 35), the Kotlin bindings (from the unstripped debug build)
      and a host library for the app's JVM tests, into `dist/`.
    - **Checked:** store unit tests (projection, waiting edits, an answer
      racing a newer edit, refusals, the full-sync sweep, a collection
      going away); the engine over real HTTP against the server on
      Postgres (`crates/server/tests/client_pg.rs`): 1,230 objects in
      batches, a quiet run is one request, edits and deletes both ways, two
      phones and the calendar page's API on one event (both phones end on
      the server's copy, the loser is reported, nothing duplicated), read
      access (refused and undone, a reminder stays personal), the network
      failing half way, a restore with `resync`, a removed phone; and the
      Kotlin bindings compiled and run on the JVM against the host
      library, with a Kotlin transport.

49. **The calendar bridge (phase 4 step 5, 2026-11-09):
    `calzone_core::android`, `calzone_client::bridge`, and the app's
    `bridge/CalendarBridge.kt`.** Built on decisions 5, 8, 9, 14, 45 and 48.
    - **The mapping is the core's, and pure** (the phone's zone and the
      clock are passed in). `rows` projects a stored event into Android's
      rows: one for the event, one per moved or canceled occurrence. It
      follows DAVx5's synctools (GPL-3.0, credited in the module): UTC
      milliseconds with an IANA zone id; all-day at UTC midnight in `UTC`;
      floating times in the phone's zone; a zone only the object's
      VTIMEZONE knows becomes UTC; DTEND for a single event, DURATION as
      `P<n>S` or `P<n>D` for a series (the provider crashes on `PT<n>S`);
      RDATE and EXDATE as UTC lists (`…T000000Z` for all-day), the start
      first among RDATEs, RDATEs dropped next to an endless RRULE;
      exceptions keyed by their original instance time; the person's own
      display reminders in minutes before the start. Tasks and journals
      are not projected.
    - **Edits by meaning.** `apply` compares an edited row with what
      `rows` wrote, field by field: text, the times as instants (DURATION
      parsed the way Android writes it), RRULE with parts and lists sorted
      and defaults dropped, the reminder set. Only what changed goes
      through `edit` (whole event or "only this one"), so everything else
      stays byte for byte. A moved time keeps its kind (floating stays
      floating, a zone stays its zone, all-day stays a date). A canceled
      exception row, a deleted exception row, or an EXDATE another app
      added deletes that occurrence. A new row becomes a new event
      (`<uuid>.ics`, an empty title gets the app's "(untitled)"); a row
      moved to another of our calendars becomes a delete in one and a
      create in the other. Availability and access level are written but
      not read back yet.
    - **Recurrence on the phone (the exception to decision 9 the plan
      asked to record):** Android expands RRULE, EXDATE and exceptions
      itself for the apps that read its tables, as it does for DAVx5.
      CalZone's own screens (4b) expand with the core.
    - **The order of a run:** Android's dirty and deleted rows become
      pending edits, the engine syncs, then the server's changes (the
      store's local change log) are written into Android. A row another
      app is editing (still dirty) is never overwritten; it is read on the
      next run. Writes use the sync-adapter URIs and select by our
      calendar ids.
    - **Calendars** are the calendar collections this phone shows, with
      the person's own color and name, read-only where they may only read
      or it is a subscribed feed. Switching one off removes it from
      Android (after its edits were read). A new calendar, a new mapping
      version, or a change of the phone's zone writes everything again.
    - **Checked:** the household corpus in two phone zones (185 events,
      36 series): an unchanged row is never an edit, a title edit changes
      only the title, and moving each series' first occurrence the way
      Fossify does lands as one override that reads back unchanged. Unit
      tests for zones, all-day, floating, RDATE, EXDATE, canceled
      exceptions, Fossify's equal forms, rules and reminders; the client
      for edits, moves, new and deleted events.
50. **The contacts bridge (phase 4 step 6, 2026-11-09):
    `calzone_core::android_contacts`, `calzone_client::bridge`, and the
    app's `bridge/ContactsBridge.kt`.** Built on decisions 5, 39, 45, 48
    and 49, the same way as the calendar.
    - **The mapping is the core's, and pure.** `row` projects a stored
      vCard into one raw contact, after DAVx5's vcard4android (GPL-3.0,
      credited in the module): the structured name with FN as the shown
      name; ORG (its parts joined) with TITLE; phones, emails and
      addresses with Android's type numbers (`TYPE=work,fax` is
      `TYPE_FAX_WORK`); a custom label (`item1.X-ABLabel`, which DAVx5
      and Apple write; 113 entries in the household's cards) is
      `TYPE_CUSTOM` with that label, Apple's own labels without their
      marks; the birthday as `yyyy-MM-dd` or `--MM-dd`; the note;
      CATEGORIES as groups; an inline photo. A photo given as a link,
      group cards (`KIND:group`) and everything else stay in the stored
      text only.
    - **Edits by meaning.** `apply` compares an edited raw contact with
      what `row` wrote: entries as sets of type, label (custom types only)
      and value, so Android's order is no change; an entry still there
      keeps its own vCard line, byte for byte, label included; only new
      ones are written (a labeled one as the next free `itemN` group).
      Parts of the name changed: N is rewritten and FN follows when it was
      made from them; only the shown name changed: FN. A card without N
      parts is not edited by the parts Android takes apart from its shown
      name. A contact made in another app becomes `<uuid>.vcf`; with no
      name it is shown as its organization, a phone number, an email
      address or "(no name)".
    - **The core card edit grew for it** (decision 39 still holds): the
      other N parts (middle, prefix, suffix), TITLE, CATEGORIES (a set),
      a photo (inline, or removed), a birthday without a year (`--MMDD`
      in 4.0, `--MM-DD` in 3.0), a chosen FN, and labels: a change without
      a label (the web's) keeps a line's label, `Some("")` asks for a line
      without one, and a removed line takes its label line along. A data
      URI whose comma is escaped (`base64\,`, six household photos) now
      reads as a photo.
    - **Checked:** all 1412 household cards: an unchanged raw contact is
      never an edit, also in reverse order; a note edit or a new phone
      loses no line and reads back unchanged; removing the last phone
      removes exactly its line (and its label). Unit tests for types,
      labels, names, dates, groups, photos and new contacts; the client
      for edits and new contacts.
51. **S6 result (phase 4b, 2026-11-11): CalZone rings its own reminders,
    from the core's occurrences, with exact alarms.** Checked with a
    throwaway APK on a Pixel 10 Pro (Android 17, GrapheneOS), then
    deleted.
    - **Exact alarms work without asking:** `USE_EXACT_ALARM` is granted
      to a calendar app; `setExactAndAllowWhileIdle`, `setAlarmClock` and
      `setExact` all arrived to the second (a 1-minute window: 38 s
      late), also as a snooze. The app uses `setExactAndAllowWhileIdle`:
      `setAlarmClock` would show an alarm icon and put reminders in the
      clock's "next alarm", next to the owner's real wake-up alarm. One
      alarm is pending at a time, for the next due moment; when it rings,
      every reminder due then is shown and the next one is set, so
      Doze's limit on idle alarms never applies.
    - **Reboots:** alarms are gone after a reboot. `LOCKED_BOOT_COMPLETED`
      arrives at once, before the first unlock; the spike put its alarms
      back then and they rang on time while the phone was still locked.
      `BOOT_COMPLETED` arrives only at the first unlock (here 10 minutes
      later). Before that unlock the store (credential-encrypted) cannot
      be read, so the app keeps the next due times in device-protected
      storage, **times only, never titles**: a reminder before the first
      unlock says "a reminder at 09:00; unlock to see it", and is
      replaced by the full one at unlock. On GrapheneOS that storage is
      readable without the PIN, so no content goes there.
    - **Android's own calendar reminders are not used.** The provider
      broadcasts `EVENT_REMINDER` for the bridge's `Reminders` rows, but
      leaves an alert it rang `scheduled` until some app dismisses it,
      and broadcasts it again after a reboot: the spike saw a reminder
      for midnight arrive at the next boot, nine hours late. Ringing from
      the core instead keeps decision 9 (recurrence is the core's) and
      works for calendars this phone hides from other apps. The bridge
      still writes `Reminders` rows, so other apps show the bell; while
      Fossify is installed it may ring as well, which ends when 4b's exit
      removes it.
    - **A plain notification,** high importance, category reminder, with
      Snooze and Open. No full-screen intent (allowed on GrapheneOS, but
      a reminder is not an alarm clock). The notification permission is
      asked when the first reminder is set, not at sign-in.
    - **Not yet seen on the phone:** a reminder after hours of Doze, and a
      time zone change (`TIMEZONE_CHANGED` arrives for manifest
      receivers, documented). Both are checked in step 5, the reminder
      step.
52. **The core and the sync API for the phone's screens (phase 4b step
    1, 2026-11-11).**
    - **`calzone_core::view`** is the calendar as a person sees it,
      moved out of the server's `domain::events` so the web and the phone
      run the same code: occurrences in the viewer's wall-clock time,
      one event as the editor shows it (`detail`), the editor's input as
      an `EventChange` (`change_for`: each event keeps its own kind of
      time, decision 37), quick add's draft, the reminder presets, the
      default color and owner initials.
    - **Repeats with an end:** `Repeat { freq, until | count }` builds
      the RRULE. `UNTIL` is the series start's own form at the start's
      time on the chosen day, so that day keeps its occurrence. The
      editor reads back `FREQ=X` with `INTERVAL=1`, `WKST`, `UNTIL` or
      `COUNT`; anything else is `custom` and kept as it is. The web panel
      has a "Stops" field (never, on a day, after a number of times); the
      API takes `repeat` next to the old `rrule`.
    - **Recur's fix 5:** calcard expands in wall time and compared a UTC
      `UNTIL` as if it were one, so a zoned series east of UTC lost its
      last occurrence when `UNTIL` is that occurrence's exact instant (as
      Thunderbird and our editor write it). The copy calcard reads now
      has such an `UNTIL` as the zone's wall time (IANA, else the
      object's VTIMEZONE). The household corpus has no such series.
    - **`calzone_client::calendar`:** occurrences of the calendars this
      phone shows (switched off here: left out), with the person's own
      name and color per calendar and the owner's initials; the event
      sheet; create (the calendar's default reminder applies), update
      with scope, remove; a reminder-only change is allowed on a view-only
      calendar, any other write there is refused on the phone as the
      server would. Each write returns what it replaced; `undo` restores
      it unless something changed it since. App edits go through
      `Store::edit_here`, which also logs a local change, so Android's
      calendar shows them at once, offline too.
    - **Expansion cache:** calcard has no "from", so an open-ended series
      costs its whole history on every call: a month of the household's
      185 events took about 140 ms on a desk machine (36 series hit the
      1024 limit). `Expansions` keeps each event's occurrences over a
      window (400 days back, 800 ahead), keyed by its text and the
      phone's zone: a cold month 129 ms once per process, then 0.7 ms a
      month (release build, desk machine). The app warms it in the
      background at start (step 2).
    - **The person's own view from the phone:** `PUT
      /sync/v1/collections/{id}/mine` replaces it as a whole (the web's
      calendar settings sheet does the same; push tells their other
      devices). The phone keeps a change in `collections.mine` (store
      schema 2) and shows it at once; the next sync run sends it before
      listing, and drops it when the server refuses it (400, 404). Week
      start, 24-hour clock, zone and language follow the phone's own
      settings, not the web's.
    - **FFI:** `occurrences`, `event`, `create_event`, `update_event`,
      `delete_event` (JSON input as the web API's, scope `this`,
      `following`, `all`), `undo` (one write at a time, by id),
      `quick_add`, `set_mine`.
    - **Known gap:** a "this and following" change made on the phone
      makes the new series as an upload; the server does not know it
      came from a split, so other people's series-wide reminders stay
      with the old series (the web carries them over, decision 37).
      Fixed when it matters, in the server's upload.

## Roadmap

Order (settled 2026-11-07): 1, 2, 4a (with moving in off Baikal onto
CalZone with DAVx5 and Fossify alongside it), 4b, 3, 5. The household test
of decision 12 moved from phase 2 to 4b: it is done on the app.
The server only matters if the household lives in it, so the app comes
before Hermes; and 4a's new sync is proven under Fossify before CalZone's
own screens are built on it. Phase numbers stay as they are.

| Phase | Builds | Replaces | Done when |
|---|---|---|---|
| 1 | core, storage, domain service, CalDAV/CardDAV door, admin web pages, Baikal import, export | Baikal | DAVx5 + Fossify and Thunderbird sync on both phones with no duplicates or lost edits |
| 2 | sharing UI, per-user layer UI, the calendar and contacts web app, ICS publishing and subscriptions | (new) | two people share a calendar, and the web app passes decision 12 |
| 3 | MCP door, assistant accounts, MCP access per collection | external caldav-mcp | Hermes reads and writes within what it was given only |
| 4a | merge in the core, sync API, push, `crates/client` + `crates/ffi`, Android store + system bridge | DAVx5 | Fossify and the phone's contacts run on CalZone's bridge with DAVx5 uninstalled, for a few weeks |
| 4b | Android calendar UI, the app's own reminders, widgets | Fossify | the household test (decision 12) on the app, then daily use for a few weeks |
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
- **Conditional writes are honored:** `If-Match` / `If-None-Match` are
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
