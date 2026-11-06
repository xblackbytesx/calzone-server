"""A second, independent client: the python `caldav` library against a
running CalZone (docs/plan-phase-1.md step 4).

    pip install caldav
    python tests/interop/caldav_client.py http://127.0.0.1:8080/ USER APP_PASSWORD

The user needs no collections; the script creates and deletes its own.
Exits non-zero on the first failed check.
"""

import sys
from datetime import datetime, timedelta, timezone

import caldav

EVENT = """BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//CalZone interop//EN
BEGIN:VEVENT
UID:interop-1@calzone.test
DTSTAMP:20260101T000000Z
DTSTART:20260310T090000Z
DTEND:20260310T100000Z
RRULE:FREQ=WEEKLY;COUNT=3
SUMMARY:Interop weekly
BEGIN:VALARM
ACTION:DISPLAY
DESCRIPTION:Reminder
TRIGGER:-PT10M
END:VALARM
END:VEVENT
END:VCALENDAR
"""


def check(ok, what):
    if not ok:
        print(f"FAIL: {what}")
        sys.exit(1)
    print(f"ok: {what}")


def main(url, user, password):
    client = caldav.DAVClient(url=url, username=user, password=password)
    principal = client.principal()
    check(principal.url.path.rstrip("/").endswith(f"/principals/{user}"), "principal discovered")

    for cal in principal.calendars():
        if str(cal.url).rstrip("/").endswith("/interop"):
            cal.delete()
    cal = principal.make_calendar(name="Interop", cal_id="interop")
    check(any(str(c.url) == str(cal.url) for c in principal.calendars()), "MKCALENDAR, listed in the home")

    ev = cal.save_event(EVENT)
    check(len(cal.events()) == 1, "PUT, then listed")

    start = datetime(2026, 3, 17, tzinfo=timezone.utc)
    hits = cal.search(start=start, end=start + timedelta(days=1), event=True, expand=False)
    check(len(hits) == 1, "calendar-query time range finds the second instance")
    hits = cal.search(start=datetime(2026, 3, 11, tzinfo=timezone.utc), end=datetime(2026, 3, 16, tzinfo=timezone.utc), event=True, expand=False)
    check(len(hits) == 0, "calendar-query skips the gap between instances")

    sync = cal.objects_by_sync_token(load_objects=False)
    token = sync.sync_token
    ev.load()
    ev.data = ev.data.replace("SUMMARY:Interop weekly", "SUMMARY:Interop weekly, edited")
    ev.save()
    changed = cal.objects_by_sync_token(sync_token=token, load_objects=True)
    objs = list(changed)
    check(len(objs) == 1 and "edited" in objs[0].data, "sync-collection reports the edit")
    check("BEGIN:VALARM" in objs[0].data, "own alarm served back")

    same = cal.event_by_uid("interop-1@calzone.test")
    check("edited" in same.data, "find by UID (prop-filter)")

    token = changed.sync_token
    same.delete()
    gone = cal.objects_by_sync_token(sync_token=token, load_objects=False)
    check(len(list(gone)) == 1 and len(cal.events()) == 0, "DELETE, tombstone in sync")

    cal.delete()
    check(all(str(c.url) != str(cal.url) for c in principal.calendars()), "calendar deleted")
    print("all checks passed")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        print(__doc__)
        sys.exit(2)
    main(*sys.argv[1:])
