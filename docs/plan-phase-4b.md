# Phase 4b plan: CalZone's own calendar on the phone

Goal: CalZone's app replaces Fossify. Calendars are read and edited in
the app's own screens, reminders ring from the app, and a widget shows
what is coming. Contacts stay in the system Contacts app (decision 14).

Done when (roadmap): the household test (decision 12,
`docs/household-test.md`) passes on the app, and the household uses it
daily for a few weeks with Fossify uninstalled.

Starts from 4a: the store, the sync engine, push and both bridges
(`docs/plan-phase-4.md`). 4a's exit (a few weeks on the bridge with DAVx5
gone) runs alongside the first steps here; steps 5 and 7 need it done.
The contract is `AGENTS.md` (decisions 5, 8, 9, 14, 37, 38, 40) and the
app's own `AGENTS.md`; this plan does not override them.

## In scope

- **Views:** agenda (the phone default, as on the web), day, week and
  month, with today, swipe to page and a date picker. Occurrences come
  from the core (decision 9), never from Android's calendar.
- **The event sheet:** read first, edit on tap. Title, calendar, all
  day, start and end, repeat, place, notes, my reminders. A recurring
  event asks the three-way scope (this one, this and later, all), as on
  the web. Quick add in English and Dutch through the core's parser.
- **Repeat with an end:** "until a date" or "a number of times". The
  web's editor lacks it too (it offers presets and "as set elsewhere");
  the household test's swimming lesson needs it, so both get it.
- **Undo** for 10 seconds after every write, as on the web (no confirm
  dialogs).
- **Shared calendars** in their color with the owner's initials, and my
  own name, color, visibility and default reminder per calendar (my
  layer, decision 8), changed from the phone too.
- **Conflicts in place:** the event shows what the merge kept, next to
  today's list on the main screen.
- **Reminders by the app:** notifications at the right time, with
  snooze, also after a reboot or a time zone change.
- **Widgets:** an agenda list, and a month grid.
- **Other apps:** "add to calendar" from a ticket or a mail, and opening
  an event from the system (search, a widget, a notification) lands in
  the app. Search in the app.

## Out of scope

Drag to move in the grids (the web has it; on a phone the sheet is
enough, revisit after the household test); invitations and free/busy
(phase 5); tasks (decision 43); a contacts UI (decision 14); tablet
layouts.

## How it fits

```
 CalZone's screens and widgets ──► the core: occurrences, edit, quick add
          │                                (through crates/ffi)
          ▼
 crates/client store ── pending edits ── sync API ── server
          │
          └─ the bridges: Android's calendar and contacts, for the dialer,
             messengers and any other app (4a, unchanged)
```

The screens read the store, never Android's calendar: that is a
projection for other apps. An edit in the app is a pending edit like
any other; the calendar bridge writes the result into Android afterwards.

## Step 0: spike S6, reminders (decide, then delete the spike code)

Two apps can ring for the same event today: the bridge writes my
reminders into Android's calendar, and whatever calendar app is
installed shows them (Fossify does). Questions for a throwaway APK on
GrapheneOS:

1. Exact alarms: does `USE_EXACT_ALARM` (granted to calendar apps without
   asking) fire on time in Doze, on GrapheneOS, for a minute-exact
   reminder? `setExactAndAllowWhileIdle` versus `setAlarmClock`.
2. Who rings: keep writing `Reminders` rows (other calendar apps and
   widgets show the bell) and let CalZone ring from its own schedule,
   while Android's own `EVENT_REMINDER` broadcast has no listener once
   Fossify is gone? Or stop writing them? Watch for double notifications
   while Fossify is still installed.
3. Reboot, time zone change, clock change and app update: which
   broadcasts arrive on GrapheneOS (`BOOT_COMPLETED`,
   `TIMEZONE_CHANGED`, `TIME_SET`, `MY_PACKAGE_REPLACED`).
4. Notification permission flow on first use, and a full-screen intent
   versus a plain high-priority notification.

Outcome recorded as a decision; the spike code is deleted.

**S6 done 2026-11-11** (decision 51): CalZone rings its own reminders
from the core with `setExactAndAllowWhileIdle` (on time to the second),
one alarm at a time; Android's calendar reminders are not used. After a
reboot the alarms are set again before the first unlock, from times kept
in device-protected storage without titles. Spike code and APK deleted.

## Step 1: the core and the sync API for screens

- **`crates/client`:** occurrences in a range with my layer applied
  (color, name, initials of a shared calendar's owner, my reminders),
  expanded by the core in the phone's zone; one event as the sheet shows
  it; the core's edit with scope, delete with scope, quick add and
  new event, each a pending edit; undo kept locally (the replaced text,
  written back as a new edit while the 10 seconds last).
- **Repeat ends:** `EventChange` gains until and count; the web editor
  gets the same fields.
- **The sync API:** writing my layer from the phone (color, name,
  hidden, default reminder), one endpoint, pushed like the rest.
  Preferences (week start, 24-hour clock, time zone, language) follow the
  phone's own settings, not the web's.
- **FFI** for all of it; golden files for the new endpoint.
- Measured on the household corpus: a month of occurrences on a phone
  well under one frame (S4 measured 7 ms on the host).

## Step 2: the app shell and the agenda

- The calendar becomes the main screen; today's settings screen moves
  behind a menu (account, sync, push, which collections show, appearance).
- Agenda: days with their events, all-day first, past days fade;
  infinite scroll both ways; today button.
- Read-only event sheet. Conflicts in place.
- Screenshot tests on made-up data, English and Dutch, light and dark.

## Step 3: day, week and month

- Day and week: a time grid with all-day row, overlapping events side by
  side, now line, swipe to page. Month: dots or short titles per day,
  tap a day for its list.
- The view and date survive rotation and process death.

## Step 4: editing

- The event sheet's edit mode, new event ("+", or a long press on a
  time), quick add, repeat with ends, the scope question, delete, undo.
- Reminders: my own, default from the calendar's setting.
- My layer per calendar from the menu: name, color, hidden, default
  reminder.
- The household test's tasks 2 to 7 and 9 can be done in the app.

## Step 5: reminders

Decision 51: one exact alarm for the next due moment, set from the core's
occurrences and my reminders; notifications with Snooze and Open; set
again after every sync, reboot (before the first unlock, from times only),
zone or clock change and app update; the notification permission asked
with the first reminder. Device checks: a reminder after hours of Doze,
and a time zone change.

## Step 6: widgets, other apps and search

- Glance widgets: agenda list and month, in the app's colors, tapping
  through to the event.
- `ACTION_INSERT` and `ACTION_VIEW`/`ACTION_EDIT` on Android's event URIs,
  so "add to calendar" and the system open the app (the bridge's
  `_SYNC_ID` maps a row back to the object).
- Search over titles, places and notes in the store.

## Step 7: the household test and the release

- `docs/household-test.md` rewritten for the app (task 10 becomes "does
  it show on your partner's phone"), and the owner runs it.
- Everything she trips over becomes a fix; a rerun of the stuck tasks
  goes without help.
- A release, then Fossify uninstalled on both phones, then a few weeks
  of daily use. That is 4b's exit.

## Exit criteria

1. The suites pass: core and client tests on the corpus, the sync API's
   golden files, the app's contract and screenshot tests.
2. The household test passes on the app (decision 12).
3. Reminders ring once, on time, on the right phone only, also after a
   reboot.
4. Fossify is uninstalled on both phones for a few weeks: nothing
   duplicated, lost or ringing twice.

## Needed from the owner

- 4a's switch on both phones (`docs/switch-over.md` in the app repo).
- The list of what annoys you both in Fossify, from moving in: it shapes
  steps 2 to 4 more than anything here.
- The S6 spike APK on a phone, and device checks after steps 2 to 6.
- The household test with your wife (step 7).
