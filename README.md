# CalZone

Shared calendars and contacts for a household, self-hosted. A web app that
is easy enough for everyone at home, and a CalDAV/CardDAV server so phones
(DAVx5) and Thunderbird keep syncing as they always did. Built to replace
Baikal.

[![CI](https://github.com/xblackbytesx/calzone-server/actions/workflows/ci.yml/badge.svg)](https://github.com/xblackbytesx/calzone-server/actions/workflows/ci.yml)
[![Release](https://github.com/xblackbytesx/calzone-server/actions/workflows/release.yml/badge.svg)](https://github.com/xblackbytesx/calzone-server/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/xblackbytesx/calzone-server?sort=semver)](https://github.com/xblackbytesx/calzone-server/releases/latest)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![Docker image](https://img.shields.io/badge/ghcr.io-calzone--server-2496ED?logo=docker&logoColor=white)](https://github.com/xblackbytesx/calzone-server/pkgs/container/calzone-server)

![The week view: calendars beside the grid, each in its own colour, with initials on events from someone else's calendar](docs/screenshots/week-dark.png)

<table>
  <tr>
    <td><img src="docs/screenshots/month-light.png" alt="Month view in the light theme"></td>
    <td><img src="docs/screenshots/event-dark.png" alt="The event panel: title, times, repeat, place, notes and your own reminders"></td>
  </tr>
  <tr>
    <td align="center">Month view, light theme</td>
    <td align="center">Editing an event</td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/feeds-dark.png" alt="Calendar settings with a new busy-only subscription link and its copy buttons"></td>
    <td><img src="docs/screenshots/contacts-dark.png" alt="The contacts list with search and letter groups"></td>
  </tr>
  <tr>
    <td align="center">Secret subscription links</td>
    <td align="center">Contacts</td>
  </tr>
</table>

<p align="center">
  <img src="docs/screenshots/phone-agenda-dark.png" width="260" alt="The agenda on a phone">
  &nbsp;
  <img src="docs/screenshots/phone-day-dark.png" width="260" alt="The day view on a phone">
</p>

<sub>Screenshots show a made-up household.</sub>

- **A calendar and contacts web app**: agenda, day, week and month views
  with drag to move and resize, quick add ("dentist thu 3pm", also in
  Dutch), undo after every change, recurring edits for one, following or
  all occurrences, and a contacts book with search. English and Dutch,
  dark by default, works on a phone.
- **People, groups and sharing**: everyone has their own calendars and
  address books and shares them with a person or a group as "can view",
  "can edit" or "can edit and share".
- **Your reminders stay yours**: on a shared calendar each person keeps
  their own alarms; nobody gets anyone else's reminders.
- **CalDAV and CardDAV** for DAVx5, Thunderbird and other standard apps,
  with discovery from the bare server address, sync tokens and conditional
  writes, so edits are never lost or doubled. It can be switched off.
- **One password per device**, shown once, optionally limited to some
  calendars, and revocable on its own.
- **Lossless storage**: events and contacts are kept exactly as clients
  wrote them, every property and parameter, unknown ones included.
- **ICS feeds both ways**: give a calendar secret subscription links
  (everything, or busy times only), revocable one by one; subscribe to an
  outside `.ics` or `webcal://` address, such as school holidays or a
  Google Calendar, as a read-only calendar that refreshes itself and keeps
  its last good copy when the source breaks.
- **Import and export**: copy a whole Baikal account (CalDAV/CardDAV, read
  only), upload `.ics` / `.vcf` files or a Google Takeout zip, download
  everything as one zip. Re-running an import changes nothing; anything
  that cannot be stored is kept, never dropped.
- **Accessible**: every page passes an automated WCAG 2.1 AA check in
  light and dark, on a phone and a desk; works by keyboard, and screen
  readers hear each event as title, day, time, place and calendar.
- **Safe to put online**: two-factor sign-in with any authenticator app
  (required for admins, or for everyone), Argon2id passphrases, login
  throttling, no third-party scripts, fonts or analytics, a strict CSP.

Coming next: scoped access for assistants and scripts (MCP), and an
Android app.

## Deploy

The image is `ghcr.io/xblackbytesx/calzone-server` (amd64 and arm64), with
Postgres 17 next to it. [deploy/](deploy/) has a Traefik example and the
[deploy guide](deploy/README.md): configuration, DNS, the first admin,
clients, backups.

```sh
docker compose up -d
docker compose exec calzone-server-app calzone-server user add anna "Anna"
docker compose exec calzone-server-app calzone-server user admin anna on
docker compose exec calzone-server-app calzone-server user passphrase anna
```

Moving over from Baikal: [docs/leaving-baikal.md](docs/leaving-baikal.md).

## Development

Everything runs through `make` and Docker; no local toolchain needed.

| Command | What |
|---|---|
| `make dev` | hot reload with Postgres on port 8080 |
| `make test` | all tests, Postgres integration included |
| `make lint` | rustfmt and clippy, the CI gate |
| `make image` / `make up` | build and run the production image locally |

`AGENTS.md` is the project contract: architecture, decisions and
invariants. Rust 1.92, axum, sqlx with Postgres, askama and HTMX.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
