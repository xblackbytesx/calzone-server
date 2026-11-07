# Phase 1 plan: leave Baikal

Goal: CalZone replaces Baikal for the household. Phones keep DAVx5 +
Fossify Calendar, the desktop keeps Thunderbird, and all of them talk to
CalZone's CalDAV/CardDAV door. Baikal stays running and untouched until
the exit criteria below hold in daily use.

The contract is `AGENTS.md`; this plan does not override it.

## In scope

- Workspace, build, CI gate, embedded-Postgres test harness.
- `calzone-core`: model, iCalendar/vCard, recurrence, the per-user alarm split.
- Storage and the domain service: principals, collections, objects, grants
  (schema only), the change log.
- `calzone-dav`: the CalDAV/CardDAV door.
- Admin web pages: login, users, app passwords, collections, import, export.
- The Baikal importer and full `.ics`/`.vcf` export.

## Out of scope (later phases)

The calendar and contacts web app, the sharing UI, the sync API, the MCP
door, push, the Android app, invitations.

## Step 0: spikes (decide, then delete the spike code)

**S1, parsing and recurrence (timebox 2 days).** Candidates: calcard 0.3.x,
`icalendar` + `rrule`, or our own parser.

Pass criteria:
- Lossless round trip of the corpus: every property, parameter and
  component survives parse then serialize (folding and order aside).
- RRULE coverage: the RFC 5545 examples, plus `EXDATE`, `RDATE`,
  `RECURRENCE-ID` overrides and a `THISANDFUTURE` split.
- Expansion matches across a DST change in Europe/Amsterdam.
- Unknown TZIDs fall back to the object's VTIMEZONE.
- vCard 3.0 and 4.0 round trip, including `PHOTO` and grouped properties
  (`item1.TEL`).
- Licence is GPLv3-compatible, and the crate builds for Android targets.

Corpus:
- **Public fixtures:** RFC examples, plus test files from permissively
  licensed projects, recorded with their source.
- **The household's own Baikal export:** kept under `fixtures/private/`.
  It is gitignored and never committed, because it holds personal data;
  tests that use it print `SKIP` when it is absent.
- **Captured client traffic:** real objects written by DAVx5 + Fossify and
  by Thunderbird, including Thunderbird's `X-MOZ-*` properties.

Record the choice and why as a decision in `AGENTS.md`.

**Done 2026-11-06:** our own content-line layer plus calcard 0.3.14 for
dates and recurrence; see `AGENTS.md` decision 25. The private corpus
contains no VTODO, RECURRENCE-ID overrides, EXDATE, Thunderbird `X-MOZ-*`
properties or vCard 3.0, so Step 2 adds public or synthetic fixtures for
each of those.

**S2, grid component (timebox 1 day).** Confirm vkurko EventCalendar's
license from its LICENSE file. Then build a throwaway page with week view,
drag, resize, drag to create and touch, styled with our tokens under the
strict CSP (no inline style). Pass means it feels instant, works on a phone
browser and needs no CSP exception; failing any of these points to our own
Web Component. Record the decision. The grid itself is built in phase 2.

## Step 1: workspace

- `Cargo.toml` workspace (`core`, `dav`, `server`; `ffi` joins in phase 4),
  edition 2024, rust 1.92, GPL-3.0-or-later.
- `rust-toolchain.toml`, `rustfmt.toml`, `bacon.toml`.
- `Makefile` and `docker/`, copied in shape from wordwarz-server.
- Migrations embedded and run at start.
- `.github/workflows/ci.yml` (gate on push) and `release.yml` (gate, tag
  equals version, image to `ghcr.io/xblackbytesx/calzone-server`; Gitea
  until decision 22 was revised),
  per decision 22.

**Done 2026-11-06.** Verified with fmt, clippy, the tests against embedded
Postgres 17, and a run of the binary (`/healthz` answered `ok`,
`CALZONE_DAV=off` logged, an invalid value refused). For the owner to run:
`make test`, `make up`, and the first CI run.
- Test harness: `CALZONE_TEST_DATABASE_URL`, one database per test, `SKIP`
  when unset.

## Step 2: core

- Model: `Collection` (calendar or address book), `Object` (UID, kind, raw
  text, projection), `Occurrence`.
- Parse and serialize through the S1 choice, behind an internal trait.
- Projection: start, end, recurrence bounds (open-ended rules get a far
  bound), summary, names, emails.
- Recurrence expansion for a time range, given the IANA zone database and
  the object's VTIMEZONEs; floating and all-day handled as such.
- Per-user split (decision 8): `split(object) -> (shared, user_layer)` and
  `merge(shared, user_layer) -> object`, where the user layer holds VALARMs
  and per-user client properties such as `X-MOZ-LASTACK` and
  `X-MOZ-SNOOZE-TIME`. Property test: `merge(split(x)) == x`.

**Done 2026-11-06.** Everything above, plus a fourth calcard fix (overrides
with a bumped SEQUENCE duplicated their instance). Verified: 41 core tests
(synthetic fixtures in `crates/core/tests/fixtures/`, the private corpus
with counts only), clippy, Android target checks. Known gap: input whose
folds split a UTF-8 character is not valid UTF-8, so it cannot be read as
text; the importer (step 6) must decide how to handle such bytes.

## Step 3: storage and domain service

Tables:
- `principals` (users and groups), `memberships`.
- `credentials`: passphrases, app passwords with scopes.
- `collections` (owner, kind, display name, color, sequence).
- `grants` (principal, collection, level).
- `objects` (collection, UID, name, raw text, projection, revision).
- `user_layers` (user, object, data).
- `changes` (collection, sequence, object name, deleted).

Domain service: list, get, put with an expected revision, delete,
`changes_since`, query by time range. All writes take the collection row
lock and bump the sequence in the same transaction.

ACL: phase 1 enforces owner-only access but routes every access through
the grant check, so phase 2 only adds UI. Unreachable collections answer
`404`.

**Done 2026-11-06,** except the credentials table, which moved to step 4
(app passwords) and step 5 (passphrases). Verified: 14 server tests on
Postgres covering preconditions, validation, personal alarms between two
users, 404 for strangers, read-only and group grants, the change log with
tombstones, range candidates, concurrent writers, and the household corpus
read back byte for byte.

## Step 4: the DAV door

Implement what DAVx5 and Thunderbird actually use:

- **Discovery:** `/.well-known/caldav`, `/.well-known/carddav`,
  `current-user-principal`, `calendar-home-set`, `addressbook-home-set`.
- **Reads:** `PROPFIND` depth 0 and 1, and `getetag`, `getctag`,
  `sync-token`, `supported-calendar-component-set`, `calendar-color`
  (Apple namespace, which DAVx5 reads), `displayname`, `resourcetype`,
  `current-user-privilege-set`.
- **Reports:** `sync-collection` (RFC 6578, with tombstones),
  `calendar-multiget`, `calendar-query` with a time-range filter,
  `addressbook-multiget`, `addressbook-query`.
- **Writes:** `GET`, `PUT` with `If-Match` / `If-None-Match` under the row
  lock, `DELETE`, `MKCALENDAR`, extended `MKCOL` for address books, and
  `PROPPATCH` for display name and color.
- **Auth:** HTTP Basic with app passwords, with per-IP failure throttling.

The door is off when `CALZONE_DAV=off`, and the test suite runs once in each
mode.

Testing:
- Integration tests replay request sequences captured from DAVx5 and
  Thunderbird (Step 0 corpus), anonymized, as golden fixtures.
- A python `caldav` client run in CI is a second, independent client.
- **Device matrix, run by the owner:**
  - Add the account in DAVx5.
  - Create, edit and delete events in Fossify.
  - Edit one occurrence, then "this and following".
  - Edit a contact in AOSP Contacts.
  - Make concurrent edits from two devices.
  - Repeat in Thunderbird.
  - Check for duplicates or lost edits after a day of use.

**Done 2026-11-06,** except the device matrix, which the owner runs.
Decision 28 records the choices. No captured traffic existed, so the golden
fixtures are the request bodies DAVx5 and Thunderbird send, written by hand
into `crates/server/tests/dav_pg.rs`; real captures can replace them after
the device matrix. Verified:
- 16 dav crate tests (XML, paths, bodies, filters, multistatus).
- 12 door tests on Postgres: discovery, sync with tombstones and refused
  tokens, conditional PUT and DELETE, multiget, time-range queries,
  personal alarms over DAV, sharing and 404s for strangers, scoped
  passwords, MKCALENDAR, extended MKCOL, PROPPATCH, validation errors,
  CardDAV, auth failures and throttling, and the door switched off.
- The household corpus (185 events, 1412 cards) PUT through the door and
  read back by one sync REPORT each, byte for byte.
- python `caldav` 3.7 runs `tests/interop/caldav_client.py` (10 checks)
  against the real binary, with the user and password made by the CLI.
  It is also a CI step.

Added beyond the plan: the admin CLI (users, collections, app passwords)
so the device matrix can run before step 5, and PROPFIND on `/`.

## Step 5: admin web pages

Login (Argon2id, uniform failures), users (admin creates and disables),
app passwords per scope, collections (create, rename, color, delete with a
typed confirmation), import, export. Semantic CSS layers from Converge and
WordWarZ, strict CSP. Plain HTMX pages only.

**Done 2026-11-06,** except the import and export pages, which move to step
6 with the code behind them. Decision 29 records the choices. Verified:
- 7 page tests on Postgres: login and logout, identical failures, the
  account and address budgets, the Origin check, collections (create,
  numbered slugs, escaping, edit, share and unshare, typed delete), device
  passwords that work and stop working on the DAV door, scoped ones, people
  (temporary passphrases, disable, admin, groups giving access), settings
  and passphrase changes that sign out other browsers, asset ETags.
- A unit test that no template has inline script, style or handlers.
- The real binary: CLI bootstrap, browser login, every page, a device
  password used by python `caldav` (all 10 checks).
- Headless Chromium over every page at 390 and 1280 px, light and dark: no
  CSP violations, no script errors, no horizontal overflow; the error
  flash shows on a refused form. Two layout fixes came from the
  screenshots (the typed-delete label, the color picker width).

Added beyond the plan: groups and their members on the People page,
`user passphrase` and `user admin` CLI commands for the first admin.

## Step 6: import and export

- **Baikal import:** source URL plus credentials, reading Baikal over
  CalDAV and CardDAV with `PROPFIND` and `REPORT` only, never a write.
  - Collections map one to one.
  - Objects are upserted by UID, so a re-run converges.
  - Alarms go to the importing user's layer.
  - A report lists counts and any object that failed to parse; the failure
    is kept as raw text, never dropped.
- **File import:** `.ics` / `.vcf` upload, including a Google Takeout
  calendar export (one `.ics` per calendar). Test it with a real Takeout
  file before the exit check.
- **Export:** one zip per user with an `.ics` per calendar and a `.vcf` per
  address book, alarms merged back in. Covered by a round-trip test:
  import, then export, equals the source (modulo folding).

**Done 2026-11-06** (decision 30). Everything above, plus a per-collection
download, the import report page, CLI `import file|baikal` and `export`,
and the UTF-8 fold gap from step 2 closed (split folds repaired, other bad
bytes kept whole as a failure). Verified: the Baikal reader against our own
DAV door over TCP, with Basic and behind a Digest gate; re-runs change
nothing; the household export files import (185 events, 1412 cards), stay
unchanged on a second run and come back out whole; headless Chromium over
the new pages at both widths and themes. Still open for the owner: a run
against the real Baikal (exit criterion 3) and a real Takeout file.

## Step 7: deploy and hand-off

- Production example in `deploy/` (decision 23): compose with Traefik
  labels and Postgres, `.env.example`, README (a first version exists from
  step 1); backups via the privacybox backup of
  `$DOCKER_ROOT/calzone-server`.
- Operator doc: config, DNS, ports, verification, the Baikal side-by-side
  procedure, and how to point DAVx5 and Thunderbird at CalZone.

**Done 2026-11-06** (decision 31). The deploy example was already in place
from step 1; this step added SIGTERM handling (the server ignored
`docker compose down`), backups, restore and upgrades in
`deploy/README.md`, and `docs/leaving-baikal.md` for the move-over
(including DAVx5 and Thunderbird). Verified: compose and workflow files
parse, the release binary builds, a SIGTERM stops the server at once.

## Exit criteria

1. The automated suites pass: core, Postgres integration, both DAV modes,
   and the python client.
2. The device matrix passes on both phones and Thunderbird.
3. A full Baikal import, re-run, changes nothing the second time.
4. A week of daily use with no duplicates, lost edits or wrong reminders.
5. Export restores into a fresh CalZone with nothing lost.

Only then do phones stop syncing with Baikal, and Baikal is retired later.

Status 2026-11-06: criterion 1 passes (125 workspace tests and the python
client). Criteria 2 to 5 need the household and follow
`docs/leaving-baikal.md` (steps 3, 1, 4 and 5 there).

## Needed from the owner

- A Baikal export or read-only credentials for the importer, for the
  private corpus.
- If available, a Google Takeout calendar export, to test the file import.
