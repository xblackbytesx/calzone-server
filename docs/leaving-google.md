# Leaving Google Calendar

How one person moves from Google Calendar to CalZone for good. Google
stays readable until the end, and a Takeout zip kept offline is the way
back.

**Before you begin: invitations.** CalZone does not send or receive
meeting invitations yet (decision 21, phase 5). Invitations sent to a
Gmail address keep landing in Google Calendar. Step 5 makes them visible
in CalZone while the person moves; step 6 decides what happens to them
after.

## Before you start

- CalZone deployed, an account for the person, signed in, two-factor on.
- The CalZone app on their phone, not signed in yet.

## 1. List what is in Google

In Google Calendar settings, under "Settings for my calendars" and
"Other calendars":

| What | Where it goes |
|---|---|
| Calendars they own (the main one, ones they made) | Takeout, step 2 |
| Calendars others share with them | a CalZone subscription, step 2 |
| Calendars they share with others | a CalZone subscription link, step 2 |
| Holidays | a public holiday `.ics` subscription |
| Birthdays | built from contacts, step 3 |
| Tasks | not a calendar; not moved |

## 2. Bring the calendars over

**Their own calendars:**

1. takeout.google.com, deselect all, select Calendar only, export.
2. Keep the zip offline. It is the backup for step 6.
3. Signed in as the person: Settings, Import & export, "Import a file",
   the zip, "Named after the file". Each Google calendar becomes one
   CalZone calendar of the same name, with the reminders in their layer.
4. Check the report: "Not imported" should be empty. Anything there can
   be downloaded exactly as it arrived.
5. Spot-check a few events: a repeating series with one changed
   occurrence, an all-day event, one in another time zone, and that
   reminders came along.

Importing the same zip again changes nothing, so it is safe to repeat.

**Calendars shared with them:** ask the owner for the calendar's secret
iCal address (Google: Settings, the calendar, "Integrate calendar"), or
use its public address. In CalZone, "Subscribe to a calendar". It is
read-only, refreshes by itself, and a failed refresh keeps the last good
copy. The person's own reminders on it still work.

**Calendars they share with others:** in the calendar's settings, under
"Subscription links", make one link per person: full detail, or "Busy
times only". Send the `webcal://` link; each one can be revoked alone.
Household members with their own CalZone account get the calendar shared
instead.

## 3. Contacts (when leaving Google Contacts too)

1. contacts.google.com, Export, vCard.
2. Import the `.vcf` the same way as the calendar zip.
3. Contacts, "Possible duplicates": merge or mark "Not duplicates".

The Birthdays calendar then builds itself from the contacts.

## 4. The phone

1. On the web: Settings, Devices, "Show a sign-in code". Scan it with the
   CalZone app.
2. Android Settings, Passwords and accounts, the Google account, Account
   sync: turn off Calendar (and Contacts, when moved). Otherwise every
   event shows twice in the system calendar and widgets.
3. GrapheneOS: the CalZone app needs the Network permission, or every
   sync waits silently.

## 5. Run side by side for 2 to 4 weeks

- New events go into CalZone only; Google is frozen.
- In CalZone, subscribe to the person's own Google main calendar by its
  secret iCal address. Invitations that still arrive in Google show up in
  CalZone, read-only, so nothing is missed.
- Tell the people who send invitations about the new address, or to send
  the details directly.

Going back is possible: turn Google Calendar sync back on in Android.
Events made in CalZone since then are not in Google; copy them over by
hand or import CalZone's export into Google.

## 6. Retire Google Calendar

Only once the safety-net subscription has stayed quiet for a while.

1. Take a second Takeout zip, kept with the first.
2. Delete the secondary calendars, then on the main calendar "Remove" or
   "Delete all events". Google Calendar itself cannot be deleted without
   deleting the Google account.
3. Keeping Gmail: Settings, "Event settings", add invitations only from
   known senders, or not at all.
4. Dropping the Google account: invitations to that Gmail address are
   lost from then on. Step 5's quiet period is the check that nobody
   still uses it.
5. Unsubscribe from the safety-net calendar in CalZone.

## What does not come over

- Google Meet links stay as text in the event; Drive attachments stay as
  links.
- Per-event colors, working locations and focus time are Google-only.
- Google Tasks.
