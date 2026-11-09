# Phase 4 plan: the Android app

Goal: both phones run on CalZone's own app. First (4a) its sync and its
system bridge replace DAVx5, with Fossify and the system Contacts app
still on top, so the new sync is proven under screens that are known to
work. Then (4b) CalZone's own calendar screens replace Fossify.

Done when (roadmap):
- **4a:** Fossify and the phone's contacts run on CalZone's bridge on both
  phones, with DAVx5 uninstalled, for a few weeks of daily use: no
  duplicates, no lost edits, offline edits merge.
- **4b:** the household test (decision 12, `docs/household-test.md`,
  moved here from phase 2) passes on the app, and the household uses it
  daily for a few weeks.

The contract is `AGENTS.md` (decisions 3, 5, 8, 9, 10, 14, 15, 16); this
plan does not override it. The Android app lives in `/workspace/calzone`
with its own `AGENTS.md`, in the kagami, slopstream and wordwarz
conventions (decision 1): Kotlin and Compose, no Play Services,
GPL-3.0-or-later.

## Alongside 4a: moving in

The household moves off Baikal onto CalZone with today's apps (DAVx5 and
Fossify on both phones) while 4a is built; it does not block the spikes.
Weeks of that prove the server, the list of what annoys people in Fossify
becomes the brief for 4b, and step 7's switch starts from it. Baikal
keeps running until then (decision 19).

## In scope

4a:
- **Core:** sync types and the three-way merge (decision 15).
- **Server:** the sync API, device sign-in by QR code (decision 10), and
  UnifiedPush (decision 16). Push moves here from 4b: how fresh the
  phone is depends on sync, not on screens.
- **`crates/client`:** the phone's store, pending changes and sync
  engine, in Rust. `crates/ffi` wraps it for Kotlin (UniFFI 0.32).
- **The app's first job (decision 14):** a CalZone account on the phone,
  a sync adapter for calendars and one for contacts, and a small settings
  screen. No calendar screens yet.

4b:
- The calendar screens, the event editor, CalZone's own reminders, and
  widgets. Planned in detail after 4a, from what moving in and 4a teach.

## Out of scope

The MCP door (phase 3, after phase 4); invitations, free/busy and the
birthdays calendar (phase 5); tasks (parked, decision 43). VTODOs and
journals sync through the store untouched. They are not put in the system
calendar, the same as DAVx5 does without a tasks app.

## How the pieces fit

```
 Fossify, widgets, dialer, messengers
          │  read and write
 CalendarContract / ContactsContract   (Android's own copies)
          │  the bridge: a projection, never the truth
 crates/client store (SQLite): full iCalendar and vCard text, my layer,
          │  pending changes, state tokens
 sync API  ◄── push says "something changed" ──  server
```

- **The store holds the truth, Android holds a projection.** Android's
  calendar and contacts tables cannot hold everything iCalendar and vCard
  can (unknown properties, custom time zones, some recurrence shapes).
  So an edit made in Fossify is read as a *change* (title, times, place,
  reminders) and applied by the core to the stored text, never written
  back from Android's row. That keeps decision 5 (lossless) on the phone.
- **Recurrence on the phone.** CalendarContract expands RRULE itself for
  the apps that read it. The bridge hands it the RRULE, EXDATE and
  exceptions as they are, as DAVx5 does today; that is Android's
  expansion for other apps, not ours. CalZone's own screens (4b) take
  occurrences from the core (decision 9). This exception to the
  invariant gets recorded as a decision in step 5.
- **My layer only.** The bridge writes my reminders, my color, my name
  and my visibility per calendar (decision 8). A reminder edited in
  Fossify goes to my layer and never rings on someone else's phone.
- **Access shows.** A calendar I may only read, or a subscribed one, is
  read-only in Android too, so Fossify does not offer edits that would
  be refused.

## Step 0: spikes (decide, then delete the spike code)

**S4, the Rust store on Android (timebox 2 days).**
- `crates/client` with rusqlite (bundled SQLite) builds next to the
  server's sqlx without a `links` clash, cross-compiles with cargo-ndk for
  arm64 and x86_64, and UniFFI 0.32 bindings load from Kotlin.
- A store of the household's size (counted from the private fixtures,
  counts only) opens, queries a month and writes a change fast enough to
  feel instant, on a mid-range phone.
- The native library adds a reasonable amount to the APK (measured, then
  recorded).
- Both phones run GrapheneOS, which follows the newest Android, so the
  app targets a recent minimum SDK (picked here and recorded) instead of
  carrying workarounds for old versions.

**S4 done 2026-11-07** (decision 44): rusqlite 0.32 next to sqlx, the
household's month in 7 ms on a desk machine, Kotlin through UniFFI on
the JVM, a 4.97 MB arm64 library, minSdk 35. The phone timings come in
step 4.

**S5, the system bridge (timebox 3 days).** Read DAVx5's synctools
(GPLv3) first and borrow with attribution (decision 1).
- A CalZone account type with a calendar and a contacts sync adapter,
  without Play Services, on GrapheneOS.
- Into CalendarContract: a timed event, an all-day event, a weekly series
  with an exception and an EXDATE, a reminder. Fossify shows each one
  correctly, and its edits come back as dirty rows that the spike can
  read as changes.
- Into ContactsContract: a contact with two phones, an email and a photo.
  The system Contacts app and the dialer show it, and an edit comes back
  as a dirty row.
- On GrapheneOS in the main profile, the phones' own setup. Private
  Space and work profiles are out: nobody keeps calendars there.

**S5 done 2026-11-09** (decision 45): every Fossify edit reads back as a
change (a deleted occurrence is a canceled exception), contacts are
editable once the account declares its fields, and Android starts our
sync within seconds of an edit, as long as the app may use the network.

## Step 1: the core, sync and merge

Pure, in `calzone-core`, shared by the server and the phone.
- Sync types: a collection's state token, a change (name, revision,
  body or tombstone), a batch.
- **Three-way merge per property** (decision 15) for iCalendar and vCard:
  base, mine and theirs. Edits to different fields both survive; one
  occurrence of a series is merged as its own unit; same field: the
  server wins and the losing value is returned, to be reported to the
  person.
- The merge never drops what it does not understand: unknown properties
  and parameters come through from whichever side has them.
- Tests: hand-written cases, property tests (merge with an unchanged side
  equals the other side), and the household corpus (fixtures only,
  counts reported).

**Step 1 done 2026-11-09** (decision 46): `calzone_core::merge` and
`calzone_core::sync`, 17 unit tests (3,000 generated edit pairs among
them) and the household corpus, events and cards.

## Step 2: the server, sync API

- **Device sign-in (decision 10):** the Devices page shows a one-time QR
  code (10 minutes), the wordwarz device-link pattern; the app scans it
  and gets a device token (only its hash stored, revocable on Devices).
  Two-factor is not asked again: the code comes from a signed-in session.
- **`/sync/v1`:**
  - the collections I can see, with my layer and my access;
  - changes since a state token per collection, in batches, from the
    one change log (decision 6), with my layer applied;
  - a batch of my changes. Each carries the copy it was based on, so the
    server merges with the core without keeping old revisions. The
    answer gives the merged result and any losing values.
- **Push (decision 16):** the app registers a UnifiedPush endpoint;
  wordwarz-server's `webpush.rs` and its rules (declared hosts only in
  `CALZONE_PUSH_HOSTS`, no redirects, `404`/`410` drops the endpoint).
  The message says only which collections changed.
- Golden JSON files for every answer (the wordwarz way), read by the
  app's contract test, so the two repos cannot drift.
- Postgres suites: two devices, offline edits on both, a web edit in
  between; nothing lost or duplicated, losers reported.
- `CALZONE_DAV=off` still works: the phone does not need the door.

**Step 2 done 2026-11-09** (decision 47): `/sync/v1` with sign-in by QR
code, change batches and merged uploads; push from database change
notices, so every door wakes the phones; golden files for the app; the
Postgres suites, push decrypted as the phone would.

## Step 3: `crates/client` and `crates/ffi`

- The store: collections, objects (full text plus the projection the
  bridge needs), my layer, pending changes, state tokens.
- The engine: pull, push pending, apply merge results, keep a list of
  conflicts to show. A failed sync keeps the last good copy and the
  pending changes.
- `crates/ffi`: thin, JSON in and out, `flat_error` for real errors
  (the wordwarz and quantifyme shape). `make android-core` builds it.
- Tests: the engine against the real server in-process, over HTTP, on
  Postgres. Two engines and the web API on one calendar.

**Step 3 done 2026-11-09** (decision 48): the store, the engine (HTTP
through the app's own client), `calzone-ffi` with `make android-core`;
checked over real HTTP against the server on Postgres, and the Kotlin
bindings run on the JVM. Added: `calzone-server resync` after a restore.

## Step 4: the app, account and sync

- `/workspace/calzone` scaffolded with its own `AGENTS.md`.
- Sign-in: scan the QR code (CameraX, ZXing), a camera-app deep link, or
  paste (the wordwarz options).
- Syncing: one engine run at a time, started by Android's sync adapter
  (an edit in Fossify or Contacts, Fossify's refresh, daily) and by
  WorkManager (a push, "Sync now"). A run that finds nothing to do is
  cheap. Unmetered or not does not matter here (small changes).
- Network: the app asks for internet from its first version; settings
  say plainly when GrapheneOS's Network permission is off, because
  Android then holds every sync without an error (decision 45).
- Settings: the account, which calendars and address books this phone
  shows, last sync, conflicts (what changed, whose value won), sign out.
- English and Dutch from the start (decision 35), dark by default
  (decision 36).
- Robolectric screenshots and the API contract test, like wordwarz.

**Step 4 done 2026-11-09** (app `AGENTS.md`, decisions 1 to 12):
`/workspace/calzone` with QR sign-in (scan, camera link, paste), the
CalZone account with both sync adapters, one engine run at a time from
Android's triggers and WorkManager, UnifiedPush, the settings screen
(conflicts in plain words, which collections this phone shows), the
GrapheneOS network warning, English and Dutch, dark by default. 12 JVM
tests (golden files, the real core over OkHttp, screenshots), lint clean.
Device-untested.

## Step 5: the calendar bridge

- Calendars into CalendarContract with my color, name, visibility and
  access; events with recurrence, exceptions, my reminders; time zones
  from the event (floating times take the phone's zone, as in DAVx5).
- Dirty and deleted rows become pending changes through the core edit
  (title, times, place, notes, reminders, recurring scope). A canceled
  exception becomes an EXDATE; RRULE and DURATION are compared by
  meaning, because Fossify rewrites them on any series edit; an event
  may end when it starts. Writes select by our calendar ids (decision
  45).
- Moving an event between calendars in Fossify: a delete in one and a
  create in the other, sent together.
- Record the recurrence exception (see "How the pieces fit") as a
  decision.

**Step 5 done 2026-11-09** (decision 49; app decisions 13 to 16): the
mapping in the core (`calzone_core::android`), edits by meaning, the
bridge around every sync run in the app; checked on the household corpus
in two zones. Device-untested.

## Step 6: the contacts bridge

- Address books into ContactsContract: names, organization, phones,
  emails, addresses, birthday, notes, photo, groups (categories).
- The account declares its fields (an EditSchema, DAVx5's list), or the
  Contacts app treats it as read-only. Photos go in as the display-photo
  file, the DAVx5 way (decision 45).
- Edits in the system Contacts app become pending changes through the
  core card edit; what Android cannot hold stays in the stored vCard.
- The dialer and messengers show names for incoming calls.

**Step 6 done 2026-11-09** (decision 50; app decisions 17 to 21): the
mapping in the core (`calzone_core::android_contacts`), the card edit
extended with labels, name parts, title, groups, photos and birthdays
without a year, the bridge around every sync run in the app; checked on
all 1412 household cards. Device-untested.

## Step 7: the switch on both phones

- An APK release on GitHub (the wordwarz way); F-Droid later.
- A switch-over guide in `docs/`: install, scan the code, check the
  calendars and contacts, turn DAVx5's accounts off (two accounts at once
  would show everything twice), use it for a while, then uninstall
  DAVx5. Going back is turning DAVx5 on again.
- Then a few weeks of daily use. That is 4a's exit.

**Step 7 built 2026-11-09**: calzone-server 0.6.0 carries step 6's core;
the app's `.github/workflows/release.yml` builds a signed APK from a tag,
with the core from the server tag in `gradle.properties` `czCoreRef`;
the guide is the app's `docs/switch-over.md`. The R8 release build was
checked to build; it has not run on a phone yet. Open: the switch on both
phones and the weeks of use.

## 4b: the CalZone app (outline)

Planned in detail when 4a is done. Expected: agenda, day, week and month
views with occurrences from the core; the event editor with the recurring
scope and quick add (English and Dutch, the core parser); shared
calendars shown by color and initials; my own reminders, delivered by
the app; a widget; conflicts shown in place. Contacts stay in the system
Contacts app (decision 14).

## Exit criteria (4a)

1. The automated suites pass: merge (with the corpus), the sync API on
   Postgres, the engine against the real server, the app's contract and
   screenshot tests.
2. Both phones run on the bridge with DAVx5 uninstalled; Fossify, the
   Contacts app and the dialer show everything.
3. An edit made offline on a phone, while someone edits the same event on
   the web, merges: different fields both survive, the same field is
   reported.
4. A change on the web reaches a phone within a minute through push.
5. Nothing duplicated or lost after a few weeks.

## Settled with the owner

- 4a before 4b: the new sync is proven under Fossify first, and the bridge
  is needed anyway (the dialer and other apps read Android's contacts and
  calendar).
- Phase 4 before phase 3: the server only matters if the household lives
  in it.
- Both phones run GrapheneOS; CalZone lives in the main profile. Private
  Space holds only untrusted apps, so it is not supported for CalZone.

## Needed from the owner

- Moving in (DAVx5 and Fossify onto CalZone) during 4a, at the latest
  before step 7.
- The household test, on the app in 4b.
- Device checks during the spikes and steps 5 to 7 (a build to install
  and a short list of what to look at each time).
