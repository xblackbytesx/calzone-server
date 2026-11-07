# Phase 2 plan: the calendar the household uses

Goal: the household plans its days in CalZone's own web app, on phones and
at a desk, with shared calendars, everyone's own reminders, contacts, and
`.ics` feeds in and out. DAVx5 and Thunderbird keep working through the
door and see every change made in the web app.

Done when (roadmap): two people share a calendar, and the web app passes
decision 12, which means a non-technical household member uses it without
help.

The contract is `AGENTS.md`; this plan does not override it.

## In scope

- The calendar: day, week, month and agenda views on the EventCalendar
  grid (decision 26), an HTMX event panel, drag to move, resize and create,
  recurring edits, quick add, keyboard shortcuts, undo.
- Contacts: list, search, view, edit, new, delete.
- The per-user layer in the UI (decision 8): my color, my name and
  visibility for each calendar, my reminders.
- Sharing from where people are (the calendar), owner shown by color and
  initials.
- ICS feeds both ways (decision 24): secret publishing links, subscriptions
  with an SSRF guard and the last good copy kept.

## Out of scope (later phases)

Tasks (VTODO) in the web app, the MCP door (phase 3), the sync API and the
Android app (phase 4), invitations, free/busy and the birthdays calendar
(phase 5). VTODOs keep syncing through the door untouched.

## Step 0: spike S2 (done 2026-11-06)

EventCalendar 5.18.1, decision 26. Vendored and pinned like HTMX.

## Step 1: core editing

Pure code in `calzone-core`, shared later with the Android app.

- **`edit`:** apply a change set (title, start, end, all-day, time zone,
  location, notes, recurrence rule) to an event's content-line tree. Only
  the lines that change are rewritten, so everything else stays byte
  identical (decision 25). `DTSTAMP`, `LAST-MODIFIED` and `SEQUENCE` are
  updated as RFC 5545 asks.
- **Recurring edits**, always all three (decision 12):
  - only this one: an override with `RECURRENCE-ID`;
  - this and following: the series ends the day before (`UNTIL`), and a new
    series with a new UID starts here;
  - all: the master changes, and overrides that only moved keep their
    offset.
  - Deleting follows the same three: `EXDATE`, `UNTIL`, the whole object.
- **New events** from a change set, with a fresh UID.
- **Quick add:** "dentist thu 3pm", "lunch with Sam tomorrow 12:30 1h",
  "holiday 24-28 dec". A small parser with the viewer's time zone and
  today's date passed in. The languages follow the UI language (below).
- **The grid's shape:** occurrences as JSON (id, title, start, end,
  all-day, color, calendar, owner, editable, recurring), from the
  existing expansion.

Exit: unit tests, plus a corpus test showing that edits leave every
untouched line byte identical across the household export.

**Done 2026-11-06.** Core modules:
- `moment`: the four time forms, instance keys, `UNTIL` values, and
  VTIMEZONEs generated from the IANA database.
- `edit`: change sets, all three recurring scopes for edits and deletes,
  new events, reminders.
- `quickadd`: English and Dutch.

Rules settled while building:
- A VTIMEZONE is added only when an edit writes a zoned time.
- An override that was never moved in time follows its series when the
  series shifts; one someone moved keeps its time.
- A bare number is a time only after "at"/"om", before "uur" or in a
  range, and then 1 to 7 mean the afternoon.
- Quick add uses the person's language, since words collide ("do", "van").

The grid's JSON shape moves to step 2, because it needs calendar colors
and owners from the server.

Verified:
- 19 new tests.
- Generated zones match the IANA offsets every 6 hours over three years
  for four zones.
- The household corpus: 185 events and 36 recurring series. No-op and
  title edits change no other line, only-this keeps every original line,
  delete-this removes exactly one occurrence, and all 36 splits neither
  lose nor double one.
- The core still builds for both Android targets.

## Step 2: domain and API

- Migration: a time zone per person (default from the browser on first
  login), and `collection_prefs` (person, collection: color, name,
  hidden, order, default reminder).
- `object_history`: the previous body of every web write and delete, kept
  30 days. **Undo** restores it, but only if nobody changed the object
  since (an ETag check).
- JSON endpoints for the grid:
  - `GET /api/occurrences?from&to`: every visible calendar, expanded by the
    core in the viewer's time zone, with their prefs applied.
  - Move and resize, which take the recurring scope.
  Writes go through `objects::put` with `If-Match`, so a stale browser
  never overwrites a phone's edit; it gets a notice and the fresh copy.
- HTMX endpoints for the event panel: view, create, edit, delete, each with
  the recurring scope.

**Done 2026-11-06** (decision 37), with JSON endpoints in place of HTMX
for writes. The page needs them for drags and undo anyway, and the panel
in step 3 posts to the same ones.
- Migration `0008_calendar_app.sql`.
- `domain::prefs`: personal settings and per-calendar settings.
- `domain::events`: occurrences, create, update, remove, undo.
- `web::api` and `web::i18n` (language resolution).

Found while building:
- Two writes in one transaction (a split) took the same sequence number,
  so a DAV client would have missed one. Each write now takes the next.
- A split dropped other people's series reminders on the new part; they
  are carried over now.

Verified: 159 workspace tests, including 3 API suites on Postgres, plus
the python client. The event panel's HTML views move to step 3 with the
page.

## Step 3: the calendar page

- `/calendar` becomes the home page. Views:
  - phone: agenda, day, month;
  - desk: week, month, day, agenda.
  The view and date live in the URL.
- The EventCalendar island (`static/calendar.js`), themed with our tokens
  through its `--ec-*` properties, light and dark. It renders occurrences
  and applies a drag at once. If the server refuses, the event snaps back
  with a notice.
- The event panel (HTMX): title, when, all-day, repeat (presets plus
  custom), calendar, location, notes, my reminders (my layer only). Opened
  by click or tap; on a phone it is a full-screen sheet.
- Quick add bar, and a round "+" button on phones.
- Keyboard: `t` today, `n` new, arrows or `j`/`k` to page, `1`-`4` for the
  views, `Esc` closes the panel.
- Swiping left and right pages on touch. Neighbouring ranges are preloaded.
- Undo instead of confirm dialogs: every move and delete shows a toast with
  "Undo" for 10 seconds.
- Owner visible: the calendar color, plus initials on events in calendars
  you do not own.

**Done 2026-11-06** (decision 38). Everything above. Found while
building:
- Boosted navigation never loaded a page's own head scripts, so the grid
  would not start after login. Scripts now load on every page.
- EventCalendar forced a light color scheme inside the dark page.
- Dragging one occurrence with "all of them" moved the series to that
  occurrence's date; it now shifts the series by the same amount.
- The scope dialog lost its centring to the CSS reset.

Verified:
- 161 workspace tests, including a page and panel test in both languages.
- Headless Chromium against the real binary: drag and undo, the scope
  question, panel edit, delete and undo, quick add, keyboard, phone
  agenda, light and Dutch. No CSP violations or script errors.

Left for later steps:
- The older admin pages are still English (step 5).
- Reminders on a calendar you can only view (step 5, my layer).
- The navigation takes three rows on a phone (step 5 or 7).

## Step 4: contacts

- `/contacts`: alphabetical list with search (name, email, phone),
  grouped by address book.
- View: photo, phones (tap to call), emails, addresses (link to a map
  search), birthday, organization, notes.
- Edit and new: the common fields. Lossless like events: unknown properties
  and parameters stay. New cards are vCard 3.0, the version the door
  advertises.
- Delete with undo.

**Done 2026-11-06** (decision 39). Everything above. Found while
building:
- A no-op edit rewrote names that had a stray space; fields now count as
  changed only when the edit says so.
- Cards with only a shown name (no structured name) were refused on save.
- `type=email` blocked saving any contact with a non-ASCII email address.

Verified:
- 171 workspace tests, including the card editor, two page suites, and the
  household corpus: all 1412 cards read, survive a no-op and a
  same-values save byte for byte, and keep every other line on a note
  edit.
- Headless Chromium with 1500 synthetic contacts (the household's real
  ones stay out of screenshots): a list of 1500 in about a second, live
  search, view, edit, save, undo, add, delete, undo, phone and desk, light
  and Dutch.

Left: the navigation now has both "Contacts" and "Calendars & contacts";
step 5 replaces the latter with the calendar list.

## Step 5: my layer and sharing in the UI

- A calendar list next to the grid (a sheet on phones): show or hide, my
  color, my name for a shared calendar, my default reminder.
- Share from the calendar list, using the existing sharing flow
  (decision 29), and see who has which level.
- Settings: time zone, first day of the week, 24-hour clock.

**Done 2026-11-06** (decision 40), with three items carried over from
earlier steps:
- reminders on calendars you can only view;
- every remaining page and message in Dutch;
- a phone navigation with a "More" menu.

Found while building:
- Undo needed write access, so a reader could not undo their own
  reminder.
- Long calendar names pushed the settings button out of the list, under
  the grid.
- The calendar grew wider than a phone once the list button joined the
  toolbar. The browser check now also flags any element past the screen's
  edge, not only page-wide scrolling.

Verified:
- 175 workspace tests, including reader reminders, the calendar list and
  sheet, preferences, and every page for a Dutch user with no common
  English words left.
- Headless Chromium against the real binary: hide and show, my name, a
  share from the sheet, folding the list, the phone "More" menu, and every
  admin page in light Dutch. Step 3's calendar flow re-run unchanged.

## Step 6: ICS feeds

- **Publishing:** a secret link per calendar, at full or busy-only detail,
  revocable, only its hash stored. `GET /feeds/<token>.ics` works with the
  DAV door off, sends ETag and `Last-Modified`, and never contains alarms
  or layers. A revoked or unknown token is `404`.
- **Subscribing:**
  - Add an `.ics` URL to get a read-only calendar.
  - A refresher task (every hour by default; per feed, with backoff on
    errors).
  - The fetcher is guarded against SSRF: DNS is resolved and every address
    checked (no private, loopback, link-local or multicast addresses unless
    `CALZONE_FEEDS_ALLOW_PRIVATE`), the request is pinned to the checked
    address, redirects are checked again (at most 3), and it is capped at
    10 MiB and 30 s.
  - The last good copy is kept (AGENTS invariant). A copy replaces it only
    if it parsed completely and is not empty. Then removed UIDs go, the
    rest goes through `upsert`.
- Both managed from the calendar list.

**Done 2026-11-06** (decision 41). A replace writes changed objects
straight through the store under the collection lock, not `upsert`, so a
whole refresh is one transaction.

Found while building:
- A changed refresh would have wiped the owner's own reminders on that
  event: the feed's empty alarm layer replaced theirs. Refreshes now store
  the shared part only, and a test sets a reminder on a subscribed event,
  changes it at the source and checks the reminder stays.
- A failed "Refresh now" answered 502, which the access log records as a
  server error every time an outside feed breaks; it is 422 now.

Verified:
- 185 workspace tests, among them: full and busy feeds (private events
  busy, free time and reminders left out, ETag and 304, the busy title in
  the maker's language), revocation and disabled owners as 404; a
  subscription to our own published link over HTTP that follows adds,
  changes and removals; a broken source (HTTP 500, empty, not iCalendar,
  one bad entry) keeping the last good copy with a translated error;
  read-only through the store and the API; private addresses refused.
- Headless Chromium against the real binary: make a busy link, copy it,
  subscribe to it, refresh, change the interval, the read-only panel,
  revoke (the copy keeps its events and shows the error), unsubscribe;
  desk dark and phone light Dutch, nothing past the edge.

## Step 7: polish and the household test

- Accessibility pass: focus order, labels, contrast, screen reader names on
  the grid.
- The headless Chromium matrix over every page: phone and desk, light and
  dark, no CSP violations, no overflow.
- The household test (decision 12): a non-technical household member does
  a list of everyday tasks without help, and whatever trips them up gets
  fixed. Then the phase is done.

**Accessibility pass done 2026-11-06** (decision 42); the household test
is open.

Found while building:
- White text on mid-tone event colors (3.4:1): the black-or-white choice
  used a fixed brightness cut-off instead of comparing contrast.
- EventCalendar's table roles held navigation and buttons, and every
  event was announced by its title alone.
- Short events stacked time over title and spilled out of their block;
  events showed "BD" for "Bram de Vries" (first two words) while the list
  showed "BV".

Verified:
- axe-core finds nothing over every page, view, panel and sheet at phone
  and desk width, light and dark, English and Dutch; no console, CSP or
  overflow problems; a keyboard walk (skip link, toolbar, list, events,
  Enter opens, Esc closes and returns focus). Step 3's calendar flow re-run
  unchanged. 185 workspace tests.

## Exit criteria

1. The automated suites pass, including edit round trips over the
   household corpus and the python CalDAV client.
2. Two people share a calendar: each sees the other's events in their
   color, keeps their own reminders, and an edit on the web shows up in
   DAVx5 and Thunderbird.
3. Undo works for every move and delete, and a stale edit is refused with
   a clear notice instead of overwriting.
4. A feed subscription survives a broken source (last good copy kept), and
   a published busy-only link shows no titles.
5. The household test passes.

## Settled with the owner

- English and Dutch, English the default, per person (decision 35). Quick
  add (step 1) understands both; pages are translatable from step 3 on.
- Dark by default, light or "follow the device" per person; the grid takes
  our tokens (decision 36).

## Needed from the owner

- Someone for the household test in step 7, and a list of the five things
  they do most in a calendar.
