# Leaving Baikal

How the household moves from Baikal to CalZone without losing anything.
Baikal stays untouched and running until the end, so every step can be
undone by switching a device back.

## Before you start

- CalZone deployed and verified at its own host name, next to Baikal
  (`deploy/README.md`). Example names below: `baikal.privacy.box` and
  `calzone.privacy.box`.
- A fresh backup of Baikal: `./manage.sh --backup --app baikal`.
- The first admin made, logged in, passphrase changed.
- Under People, an account for each person.

## 1. Copy each Baikal account

Import & export, "Copy from Baikal" (admins), once per Baikal account:

| Field | Value |
|---|---|
| Into the account of | the person whose reminders these are |
| Baikal address | `https://baikal.privacy.box/dav.php/` |
| Baikal user name, password | that person's Baikal login |

Or from the shell, the password on stdin:

```
cz import baikal anna https://baikal.privacy.box/dav.php/ anna
```

CalZone only reads from Baikal. Each Baikal calendar and address book
becomes one of the same name in that person's CalZone account, with its
color; reminders go to that person only.

Check the report:

- Every collection is listed with "new" counts that match what the phones
  show.
- "Not imported" should be empty. If not, download the entries from the
  report; they are kept exactly as Baikal sent them.

Then run the same copy again. Every entry must be "unchanged". This is the
first check that CalZone stores what it reads without changing it.

## 2. Share what the household shares

On Baikal the household probably shared one account. In CalZone each
person keeps their own: open the calendar under Calendars & contacts and
share it with the others ("can edit"). Everyone's own reminders stay their
own.

## 3. Move the devices, all on the same day

A re-run of the copy replaces CalZone's version of an entry with Baikal's,
and entries deleted in Baikal stay in CalZone. Both are fine while nobody
writes to CalZone yet; after that, do not copy again. So run a last copy,
then move every device the same day.

For each person, under Devices, make one password per device.

**DAVx5 (Android):**

1. In DAVx5, open the Baikal account and turn off sync for its calendars
   and address books (keep the account for now).
2. Add an account, "Login with URL and user name":
   `https://calzone.privacy.box`, the CalZone user name, the device
   password.
3. Turn on its calendars and address books, then sync.
4. In Fossify Calendar and Contacts, show the CalZone ones and hide the
   Baikal ones, so nothing appears twice.

**Thunderbird:**

1. Calendar: New Calendar, On the Network, the CalZone user name and
   `https://calzone.privacy.box`, then the device password. Pick the
   calendars it finds.
2. Address book: New Address Book, CardDAV, the same details.
3. Unsubscribe from the Baikal calendars and address books (right click,
   Unsubscribe). Nothing is deleted on Baikal.

Check on every device: an entry made on one appears on the others, and an
edit and a delete travel too.

## 4. Live with it for a week

Use only CalZone for a week. Watch for duplicates, lost edits and
reminders that fire at the wrong time or for the wrong person. Anything
odd: note what, on which device and when, before changing anything.

Going back is possible at any point in the first days: turn the Baikal
sync back on and the CalZone sync off. Edits made in CalZone since then
are not in Baikal, so copy them over by hand.

## 5. Prove the export restores

Each person, Import & export, "Download everything". Then on any machine
with the repo, start a throwaway CalZone and import the zip:

```
cp .env.example .env
make up
alias czl='docker compose --env-file .env -f docker/docker-compose-build.yml exec calzone-server calzone-server'
czl user add test && czl user passphrase test
```

Log in at `http://localhost:8080` as `test`, upload the zip under Import &
export, and compare the counts on its report with the live server. Then
`make down` and delete `data/postgres`, the throwaway database.

## 6. Retire Baikal

1. Remove the Baikal account from DAVx5 on every phone, and the Baikal
   calendars and address books from Thunderbird.
2. Keep Baikal running a few more weeks, untouched, in case something is
   missing.
3. Take a last backup (`./manage.sh --backup --app baikal`), then take it
   down and remove it from `DEPLOYED_APPS`.
