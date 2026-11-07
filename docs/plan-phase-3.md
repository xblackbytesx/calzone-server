# Phase 3 plan: Hermes and scripts through the MCP door

Goal: Hermes (and any script the owner writes) reads and changes the
household's calendars and contacts through a door built into CalZone,
with a token that reaches only what it was given. No outside CalDAV MCP
server holding a full password (decision 17).

Done when (roadmap): Hermes reads and writes within its scope only.

The contract is `AGENTS.md`; this plan does not override it.

## In scope

- Personal access tokens (decision 10): made in the web app, shown once,
  each scoped to named calendars and address books with `read` or
  `write`, revocable on its own, only a hash stored.
- The MCP door at `/mcp`: tools for calendars, events and contacts, over
  the domain service like every other door.
- A trail: every write made with a token says which token made it, and
  the person can see and undo it.
- `CALZONE_MCP` (`on` or `off`), like `CALZONE_DAV`.

## Out of scope (later phases)

The sync API and the Android app (phase 4), invitations, free/busy and the
birthdays calendar (phase 5). OAuth for the door: a token pasted into
Hermes's config is enough for one household. Tasks (VTODO) are an open
question for the owner (see the end).

## Step 0: spike S3, the MCP library (timebox 1 day)

Candidates: `rmcp` (the official Rust SDK) mounted in our axum router, or
our own small JSON-RPC handler (`initialize`, `ping`, `tools/list`,
`tools/call`; the door needs nothing else).

Pass criteria:
- Streamable HTTP, stateless: every request carries the token, no session
  state to lose on a restart.
- The bearer check is our own axum layer, so throttling and the
  `auth_failure` line (decision 34) work the same as for DAV.
- Hermes connects, lists the tools and calls one. A second client (MCP
  Inspector) does the same, so we are not fitted to one client's quirks.
- The license fits GPLv3; the dependency tree stays modest and builds
  for arm64.

Record the result as a decision, then delete the spike code.

## Step 1: the scope moves into the domain

Today the domain's `Actor` is a user id, and an app password's scope is
enforced in the DAV door (`Ctx::collection`, `Ctx::visible`). A second
scoped door must not copy that.
- `Actor` becomes the user plus an optional scope. `collections::get`,
  `visible` and every entry point that takes a collection cap access by
  it in one place; outside the scope is `NotFound`, never a hint that
  something exists.
- The DAV door moves onto it and loses its own copy.
- A table-driven test calls every domain entry point with a scoped actor
  and checks reads, writes and deletes outside the scope all fail the same
  way. New entry points join the table, or the test fails.

## Step 2: tokens

- Migration `0010_tokens.sql`: app passwords gain a `purpose` (`dav` or
  `mcp`), so a DAV password never opens the MCP door and the other way
  round. One table and one scope model; the verify path is shared.
- An MCP token is `czt_` plus 25 characters from the app password
  alphabet. The prefix lets secret scanners and the owner recognize a
  leaked one.
- A token always has a scope. There is no "everything" token: the person
  picks the calendars and address books.
- `object_history` gains a nullable `token_id`, filled for every write
  through a token.
- Verify: `Authorization: Bearer`, the same `401` for every failure, and
  the same 10-in-15-minutes budget per address as DAV.
- CLI: `cz token add|list|revoke`.

## Step 3: the door, reading

`/mcp`, off unless `CALZONE_MCP=on`. Requests with an `Origin` header
from another site are refused (DNS rebinding). Tools:
- `whoami`: the person, their time zone, now in that zone, and the
  calendars and address books this token reaches with `read` or `write`.
  Agents get dates wrong without it.
- `list_events`: a range (default today plus 7 days) and an optional text
  filter, expanded by the core (decision 9) in the person's zone. Each
  occurrence: title, start, end, all-day, place, notes, calendar,
  recurring, and the ids a write needs.
- `get_event`: one event with its recurrence in words and the person's
  reminders.
- `search_contacts` and `get_contact`: name, organization, emails,
  phones, birthday, notes. No photos.

Times go in and out as local wall time with the zone named
(`2026-11-09T14:00`, `Europe/Amsterdam`), never bare UTC, because that is
where agents slip.

## Step 4: the door, writing

- `create_event`, `update_event`, `delete_event`, with the recurring
  scope (this one, this and following, all) and `If-Match` like the web
  app. A stale write gets the fresh copy back instead of overwriting.
- `quick_add`: the phase 2 parser (English and Dutch) for "dentist
  tuesday 3pm".
- `create_contact` and `update_contact`. No contact deletes: a contact
  lost by an agent is the hardest thing to notice.
- Every write answers with its `change_id`, and `undo` takes it back
  (the phase 2 undo, with the same ETag check).
- Reminders a token sets land in the person's own layer (decision 8),
  never on the shared event.

## Step 5: tokens in the web app

- Devices gets a second section, "Assistants and scripts": new token
  (a name, then the calendars and address books with `read` or `write`
  each), shown once with a copy button and the `/mcp` address, then last
  used and revoke.
- Each token shows its recent changes (from `object_history`), each with
  undo, so "what did Hermes do this week" has an answer.
- Both languages; the step 7 audit (axe, phone and desk, light and dark)
  runs over the new pages.

## Step 6: Hermes and the hand-off

- `deploy/README.md`: a "Hermes and scripts" section (turn the door on,
  make a token, the client config, one `curl` to check), the new variable
  in `.env.example` and in privacybox.
- The roadmap test with the real Hermes: it answers "what's on tomorrow",
  adds an event to a calendar it may write, and fails cleanly on one it
  may only read and on one outside its scope.

## Safety

Event notes come from other people and from subscribed feeds, so tool
results are untrusted text in an agent's context. The door's job is to
keep the damage of a misled agent small, not to filter text:
- The scope is the boundary. A read-only token cannot write, whatever the
  agent is told.
- No tool reaches outside CalZone: no sending mail, no fetching addresses,
  no subscribing to feeds, no sharing, no tokens, no settings.
- Writes are undoable and traced to the token (steps 2 and 5).
- Busy-only details stay busy-only: what a feed link would hide, the door
  hides too.

## Exit criteria

1. The automated suites pass, including the scope table of step 1 and an
   MCP suite on Postgres that drives the door over HTTP.
2. A token reaches only its scope: out-of-scope calendars are absent from
   every answer, and reads and writes to them fail the same as a calendar
   that does not exist.
3. A write through the door shows up in the web app, DAVx5 and
   Thunderbird, and undo from the token's page takes it back.
4. Hermes passes the roadmap test of step 6.

## Settled with the owner

- A built-in door with scoped tokens, not an outside CalDAV MCP server
  (decision 17).

## Needed from the owner

- How Hermes connects to MCP servers: Streamable HTTP with a bearer
  header is the plan; if it only does stdio, step 0 adds a small bridge.
- Which calendars and address books Hermes gets, and which with `write`.
- Tasks: should Hermes list, add and complete VTODOs? Recommended yes
  (to-dos are what an assistant gets asked for most, and they already sync
  through the DAV door). It adds a step after step 4, and phase 3 is then
  the first place tasks are edited.
- Event deletes: allowed for a `write` token (with undo), or only by a
  person in the web app?
