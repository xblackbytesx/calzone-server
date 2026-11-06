# Deploying CalZone

The released image from `ghcr.io/xblackbytesx/calzone-server` (amd64, arm64),
with Postgres, behind Traefik: the web app (people, calendars and address
books, sharing, device passwords, import and export) and the CalDAV/CardDAV
door. Moving over from Baikal: `docs/leaving-baikal.md`.

## Files

```
cp deploy/docker-compose.yml deploy/.env.example <your stack>/calzone-server/
cd <your stack>/calzone-server && mv .env.example .env
```

Edit `.env`. At minimum set `POSTGRES_PASSWORD` (`openssl rand -hex 24`),
`CALZONE_SECRET_KEY` (`head -c 32 /dev/urandom | base64`; keep it with your
backups) and `CALZONE_PUBLIC_URL`.

| Variable | Meaning |
|---|---|
| `DOMAIN`, `SUBDOMAIN`, `CERT_RESOLVER` | Traefik host rule and certificate |
| `DOCKER_ROOT` | host directory for the database (`$DOCKER_ROOT/calzone-server/database`) |
| `CALZONE_VERSION` | image tag: `0.3.0`, `0.3` or `latest` |
| `CALZONE_PUBLIC_URL` | the address clients use |
| `CALZONE_DAV` | `on` or `off`: the CalDAV/CardDAV door for other apps |
| `CALZONE_COOKIE_SECURE` | `on`; `off` only for plain-http LAN use |
| `CALZONE_BEHIND_PROXY` | `on` behind Traefik: login throttling reads the client from `X-Forwarded-For` |
| `CALZONE_SECRET_KEY` | required; encrypts two-factor secrets. Lost: everyone sets two-factor up again |
| `CALZONE_TWO_FACTOR` | who must use two-factor sign-in: `everyone`, `admins` (default) or `optional` |
| `CALZONE_DEFAULT_TIMEZONE` | IANA zone for people who have not set theirs (default `UTC`) |
| `CALZONE_FEEDS_ALLOW_PRIVATE` | `off` (default): calendar subscriptions never fetch private, loopback or link-local addresses. `on` only to subscribe to a feed inside your own network |

Create the database directory, owned by Postgres's container user:

```
mkdir -p /media/storage/docker/calzone-server/database
```

## DNS and ports

One host name (`calzone.privacy.box`) pointing at Traefik. No host ports are
published; the app listens on 8080 inside the `proxy` network.

## Start and verify

```
docker compose up -d
docker compose logs -f calzone-server-app
curl -fsS https://calzone.privacy.box/healthz
```

Expect `ok`. The log line `CalDAV/CardDAV door on` (or `off`) confirms
`CALZONE_DAV` was read.

## The first admin

```
alias cz='docker compose exec calzone-server-app calzone-server'
cz user add anna "Anna"
cz user admin anna on
cz user passphrase anna
```

The last command prints a temporary passphrase once. Log in at
`https://calzone.privacy.box`, set up two-factor sign-in when asked (any
authenticator app; keep the recovery codes), change the passphrase under
Settings, then do the rest in
the browser: People (add the household; each gets a temporary passphrase to
hand over), Calendars & contacts (create, colour, share with a person or a
group), and Devices (one password per phone or app, optionally limited to
some calendars).

The same is possible from the shell: `cz collection add`, `cz app-password
add|list|revoke` (`cz help` lists them). A forgotten passphrase: an admin
picks "New passphrase" on the People page, or `cz user passphrase NAME`.
A lost phone with no recovery codes: "Reset two-factor" on People, or
`cz user two-factor-reset NAME`.

## Clients

| Client | Setting |
|---|---|
| DAVx5 | "Login with URL and user name": `https://calzone.privacy.box`, user name, app password |
| Thunderbird | New calendar, On the network: user name and `https://calzone.privacy.box`; address books the same way |
| Anything else | `https://calzone.privacy.box/dav/` |

Discovery uses `/.well-known/caldav` and `/.well-known/carddav`. Ten
failed logins from one address within 15 minutes lock that address out for
the rest of the window (`429`); the web login also allows five per account.
Each refused sign-in logs one `auth_failure` line; [crowdsec/](crowdsec/)
turns those into bans at the proxy.

```
curl -si https://calzone.privacy.box/.well-known/caldav | head -3
curl -s -u anna:<app password> -X PROPFIND -H 'Depth: 1' https://calzone.privacy.box/dav/calendars/anna/
```

Expect a `301` to `/dav/`, then a `207 Multi-Status` listing the calendars.

## Import and export

Import & export in the web app: download everything as a zip, upload
`.ics` / `.vcf` files or a Google Takeout zip, and (admins) copy a Baikal
account, which only reads from Baikal. Re-running an import changes
nothing that is already there. Each import has a report; entries that
could not be stored can be downloaded as they arrived. From the shell:

```
cz import baikal anna https://baikal.example.org/dav.php/ anna   # password on stdin or BAIKAL_PASSWORD
cz import file anna /path/family.ics
cz export anna /path/anna.zip
```

## Calendar feeds

A calendar's settings (the "⋯" in the calendar list) make secret
subscription links, full or busy-only, served at `/feeds/<token>.ics`
with or without the DAV door. "+ Subscribe to a calendar" adds an outside
`.ics` or `webcal://` address as a read-only calendar. The server fetches
it itself, so it needs outgoing HTTPS. Check a link from outside:

```
curl -sI https://calzone.example.org/feeds/<token>.ics   # 200, text/calendar
```

## Backups and upgrades

Everything CalZone keeps lives in Postgres under
`$DOCKER_ROOT/calzone-server/database`. In privacybox (where the app is
`apps/calzone`, data under `$DOCKER_ROOT/calzone`) the rolling backup stops
the app, archives that tree and starts it again:

```
./manage.sh --backup --app calzone
```

Restore: `docker compose down`, put the archived `database` directory back,
`docker compose up -d`. A portable copy besides: a SQL dump, and each
person's "Download everything" zip, which any calendar app can read.

```
docker compose exec calzone-server-db sh -c 'pg_dump -U "$POSTGRES_USER" "$POSTGRES_DB"' > calzone.sql
```

Upgrades: set `CALZONE_VERSION` (pin a release, `0.3.0`, rather than
`latest`), then `docker compose pull && docker compose up -d`. Migrations
run at start; take a backup first. The server stops cleanly on
`docker compose down`; an import cut off by a stop is marked interrupted
and can simply be run again.
