# Phase 3 plan: assistants through the MCP door

Goal: the household's assistant (Hermes, in a Matrix room with both
parents and in each one's DM) reads and adds to the calendars and contacts
it was given, through a door built into CalZone. No outside CalDAV MCP
server holding a full password (decision 17). The design is decision 43.

Done when (roadmap): the assistant reads and writes within what it was
given only.

The contract is `AGENTS.md`; this plan does not override it.

## In scope

- **Assistant accounts:** a third kind of principal next to users and
  groups. An admin makes one and names it (CalZone never says "Hermes"
  itself). It has no passphrase and no web login, only tokens.
- **MCP access per calendar and address book:** on the collection page,
  under name and color, "MCP access": Off (the default), Busy only, Read,
  Read and add, or Read, add and change. Busy only gives times and
  nothing else, so the assistant can plan around a calendar without its
  content reaching a cloud model. Only someone who may share the collection (its owner or
  a calendar admin) sets it, so nobody exposes a calendar that was shared
  with them.
- **The MCP door** at `/mcp`, Streamable HTTP with a bearer token, off
  unless `CALZONE_MCP=on`.
- **A trail:** every change the assistant makes is recorded, shown to the
  collection's owner and the admins, and can be undone.

## Out of scope

- **Deletes through the door.** Trust first. The assistant can undo its
  own changes (taking back an event it just added), and nothing else.
- **Tasks (VTODO), parked.** Nobody uses tasks yet (the Baikal export
  has none). To decide later: a separate to-do list or tasks inside
  calendars, the phones' tasks app, and tasks in the web app. VTODOs keep
  syncing through the DAV door untouched.
- **Reminders through the door.** The assistant cannot tell whose phone
  should ring. Events it adds get each person's own default reminder
  instead (step 4).
- **Recurrence rules on existing events** through the door: cutting a
  series short removes occurrences, which is a delete.
- OAuth for the door; the sync API and the Android app (phase 4);
  invitations, free/busy and the birthdays calendar (phase 5).

## Step 0: spike S3, the MCP library (timebox 1 day)

Candidates: `rmcp` (the official Rust SDK) mounted in our axum router, or
our own small JSON-RPC handler (`initialize`, `ping`, `tools/list`,
`tools/call`; the door needs nothing else).

Pass criteria:
- Streamable HTTP, stateless: every request carries the token, no session
  state to lose on a restart.
- The bearer check is our own axum layer, so throttling and the
  `auth_failure` line (decision 34) work the same as for DAV.
- Two independent clients connect, list the tools and call one: MCP
  Inspector and the official Python SDK's client. Hermes itself comes
  with the release (step 6).
- The license fits GPLv3; the dependency tree stays modest and builds
  for arm64.

Record the result as a decision, then delete the spike code.

## Step 1: assistants and the "add" level

- Migration `0010_assistants.sql`:
  - principals get the kind `assistant`, with a time zone (default
    `CALZONE_DEFAULT_TIMEZONE`);
  - grants get the levels `busy` and `add`, only for assistants;
  - app passwords gain a `purpose` (`dav` or `mcp`), so a DAV password
    never opens the MCP door and the other way round;
  - `object_history` gains the acting principal for every change.
- MCP access is an ordinary grant from the collection to the assistant,
  so the existing sharing checks (`collections::get`, `visible`) are the
  boundary. No second scope model.
- `add` may create objects and undo its own creations while nobody has
  changed them since, nothing more. The check sits under the collection
  lock: a create with a name that already exists is refused, whatever
  the request's preconditions say.
- **Assistants never delete,** at any level, in the domain itself (not
  only by the door lacking the tool). The one exception is undoing their
  own creation.
- **Busy is not read.** A `busy` grant makes the calendar visible with
  each occurrence's start and end only: no title, place, notes, repeat or
  calendar description, and `get_event` refuses. The domain does the
  stripping, so no tool or later door can leak more. Address books take
  no busy level.
- **Levels are checked per kind** in `set_grant`: an assistant takes
  busy, read, add or write, never admin (which would let it share, rename and
  delete); users and groups never take add. The subscribed-calendar lock
  drops add to read as it does write.
- **An assistant is nobody's account:** never a group member (a calendar
  shared with "the household" group would otherwise reach it silently),
  never an admin, no passphrase, no web login, no app passwords, no
  two-factor. Each of those refuses in the domain, so the CLI cannot do
  it either (`cz user admin hermes on`, `cz app-password add hermes`).
  It never appears in the share picker or People's person list, and the
  share sheet leaves its grants out (the collection page shows them).
- **Removing an assistant disables it** and revokes its tokens. Its rows
  stay, because `object_history` cascades on the principal and the trail
  (and undo) would vanish with it. Disabled also answers the "pause"
  switch: one click and every token stops.
- A table-driven test calls every domain entry point as an assistant
  with each level (none, busy, read, add, write) and as a disabled one, and
  checks what passes.

## Step 2: tokens

- A token is `czt_` plus 25 characters from the app password alphabet.
  The prefix lets secret scanners and the owner recognize a leaked one.
- Admins make, list and revoke an assistant's tokens. Shown once; only a
  hash is stored.
- Verify: `Authorization: Bearer`, the same `401` for every failure, the
  same 10-in-15-minutes budget per address as DAV, one `auth_failure`
  line each. Only purpose `mcp` tokens of enabled assistants pass; the
  DAV door refuses assistants even with a valid password.
- **A runaway brake:** at most 60 changes an hour per assistant; past it
  writes answer "slow down" until the hour turns. Reads are not limited.
- Logs carry the assistant and the tool name, never arguments or results
  (they are the household's calendar), and never the token.
- CLI: `cz assistant add NAME`, `cz assistant token NAME`,
  `cz assistant revoke NAME ID`.

## Step 3: the door, reading

Requests with an `Origin` header from another site are refused (DNS
rebinding). Tools:
- `tools/list` offers only what the assistant's levels allow (no write
  tools with read alone), and every tool carries the MCP hints
  (`readOnlyHint`, `destructiveHint: false`), so the model does not try
  what will fail.
- `whoami`: the assistant's name and time zone, now in that zone, and
  every calendar and address book it reaches, with owner and level.
  Never a subscription's source address, feed links, grants, or anyone
  else's settings.
  Agents get dates wrong without it, and in a shared room the owner names
  are how it tells "my calendar" from "her calendar".
- `list_events`: a range (default today plus 7 days), optional calendars
  and a text filter, expanded by the core (decision 9). Each occurrence:
  title, start, end, all-day, place, notes, calendar, owner, recurring,
  and the ids a change needs.
- `get_event`: one event with its recurrence in words.
- `search_contacts` and `get_contact`: the contact fields above.

**Little per call (the model may be in the cloud):**
- `list_events` covers at most 31 days and 200 occurrences per call;
  the default is today plus 7 days.
- `search_contacts` needs a search text and returns at most 20; there is
  no "list everything" and no export tool.
- Contacts come through as name, organization, emails, phones and
  birthday. Street addresses, notes and photos stay home.

**Private events are busy:** an event marked `CLASS:PRIVATE` or
`CONFIDENTIAL` comes through as "Busy" with its times only, the same rule
as a full feed link (decision 41). The web app shows them in full to
people the calendar is shared with; the door is not a person.

Times go in and out as local wall time with the zone named
(`2026-11-09T14:00`, `Europe/Amsterdam`), never bare UTC, because that is
where agents slip.

## Step 4: the door, writing

- The door takes fields (title, start, end, place, notes, a simple
  repeat), never raw iCalendar or vCard, so nothing it sends can carry
  alarms, attendees or other people's layers.
- `add` level: `create_event` (a repeat allowed: daily, weekly, monthly
  or yearly, with an end), `quick_add` (the phase 2 parser, English and
  Dutch), `create_contact`.
- Change level: also `update_event` (title, times, place, notes, with the
  recurring scope: this one, this and following, all; not the repeat
  rule) and `update_contact` (the fields it shows; the photo and
  everything else are kept, decision 5). An ETag is required: a stale
  change gets the fresh copy back instead of overwriting.
- **Reminders for the people, not the assistant:** today a new event gets
  its creator's default reminder only, so an assistant's event would ring
  nobody. An event the assistant adds gets each person's own default
  reminder for that calendar, in their own layer (decision 8).
- Every change answers with its `change_id`; `undo` takes back the
  assistant's own changes (the phase 2 undo, with its ETag check).

## Step 5: the web app

- **Collection page:** the "MCP access" choice under name and color, for
  calendars and address books. With one assistant it is a single choice;
  with several, one row each.
- **People page (admins):** an "Assistants" section: add one (a name),
  make and revoke tokens (shown once with a copy button and the `/mcp`
  address), last used.
- **The trail:** the assistant's recent changes, each with undo, on the
  People page for admins and on the collection page for anyone who may
  write to it, so "what did it do this week" has an answer. Undo today
  only takes back your own change; it learns one rule: someone with write
  access to every collection involved may undo an assistant's change.
- **The MCP access hint says what it means:** "Anyone who can talk to the
  assistant can see this calendar."
- An "Assistant paused" switch on People (step 1's disable).
- Both languages; the step 7 audit (axe, phone and desk, light and dark)
  runs over the new parts.

## Step 6: the hand-off and the release

- `deploy/README.md`: an "Assistants (MCP)" section (turn the door on,
  make an assistant and a token, stock Hermes's MCP config taken from its
  own docs, one `curl` to check), the new variable in `.env.example` and
  in privacybox.
- The same roadmap test, scripted against the door with the SDK client:
  "what's on tomorrow", an event added to a calendar set to Read and add,
  clean refusals on Read and on Off, and undo from the trail.
- Then a release. The owner deploys it on the VPS and talks to a stock
  Hermes (no changes of its own); whatever comes out of that becomes a
  patch release.

## Safety

- **Anyone who can talk to the assistant gets its access.** In the
  Matrix room that is both parents; a guest invited to the room could ask
  it about your calendars too. The MCP access choice is that decision.
- **That includes whoever holds a token.** An admin can make one, and
  read through it whatever the assistant sees. That is no new power: an
  admin can already reset anyone's passphrase and two-factor.
- **Its model sees what it reads, and it is partly in the cloud.**
  Hermes delegates between a local and a cloud model, so the plan
  assumes everything the door returns may leave the house. MCP access is
  that choice; everything starts at Off; Busy only is there for "plan
  around it, but don't read it"; and every answer carries as little as
  the question needs.
- **The same machine:** Hermes calls CalZone by its public name, so its
  requests arrive from the host's own address. A revoked token left in
  its config would make CrowdSec ban that address; the deploy docs say to
  allow the host's own address in CrowdSec.
- **Notes are untrusted text.** Event notes come from other people and
  from subscribed feeds, and land in the agent's context. The door keeps
  a misled agent's damage small instead of filtering text: the level is
  the boundary, there are no deletes, no tool reaches outside CalZone (no
  mail, no fetching addresses, no feeds, no sharing, no tokens, no
  settings), and every change is traced and undoable.

## Exit criteria

1. The automated suites pass, including the level table of step 1 and an
   MCP suite on Postgres that drives the door over HTTP.
2. Calendars set to Off are absent from every answer; reads and writes to
   them fail the same as a calendar that does not exist. No level, tool
   or argument deletes anything, except undoing the assistant's own new
   event.
3. An event the assistant adds rings on each person's phone with their
   own default reminder.
4. A calendar set to Busy only gives times through every tool and
   nothing else.
5. A change through the door shows up in the web app, DAVx5 and
   Thunderbird, and undo from the trail takes it back.
6. The scripted roadmap test of step 6 passes; after the release, stock
   Hermes does the same on the VPS (fixes come as patches).

## Settled with the owner

- A built-in door, not an outside CalDAV MCP server (decision 17).
- Hermes runs on the same public VPS; HTTP with a bearer token, behind
  the existing throttling and CrowdSec.
- One assistant for the whole household (a Matrix room with both parents,
  plus each one's DM), so an assistant account rather than personal
  tokens; access set per collection, by whoever may share it.
- No deletes for now; tasks parked.
- Hermes is stock and has no MCP servers yet; it is tried against the
  published release, and its feedback becomes a patch.
- Hermes delegates between a local and a cloud model: assume little
  privacy and be defensive.

## Needed from the owner

- After the release: deploy it, connect a stock Hermes, and report what
  trips it up.
