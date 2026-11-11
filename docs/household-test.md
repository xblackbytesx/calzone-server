# The household test

Decision 12: CalZone does not count as done until a household member who
did not build it uses it without help. Since phase 4b the test runs on
the CalZone phone app: she uses CalZone's own calendar on her phone, and
contacts in the phone's own Contacts app (decision 14). This is the kit:
set up, hand over the task cards, watch, write down, report.

## Before the test

1. CalZone 0.7.0 runs on the VPS (`deploy/README.md`), with push set up
   (`CALZONE_PUSH_HOSTS`) and two-factor set up for both of you.
2. Your real calendars and contacts are in CalZone. A test on real data
   finds real problems; an empty calendar finds none.
3. The family calendar is shared between you both with "can edit". Each
   of you has at least one calendar of your own.
4. Both phones run the CalZone app, v0.2.0 or later, signed in, and
   DAVx5's calendars and address books are switched off
   (`docs/switch-over.md` in the app repo; uninstalling DAVx5 can wait).
   Push is on under Settings, "Instant updates", on both.
5. On her phone: calendar and contacts access allowed, notifications
   allowed when the app asked, and the phone in the language she wants
   (the app follows it).
6. Fossify may stay installed for now, but she uses CalZone. While both
   are installed a reminder can ring twice, once from each; that is
   expected and ends when Fossify goes (see the end).
7. Open the app once yourself on her phone and leave it on today, in the
   view it opens in (Maand en dagen). Close it again.

## How to watch

- Hand over one card at a time. Say nothing else, and don't point.
- Write down where she looks first, where she hesitates, and what she
  says out loud ("waar is...", "huh"). Her words are the best data.
- **Stuck rule:** after 3 minutes without progress, help with the
  smallest hint that works, and mark the task "helped".
- Mark each task: **alone**, **hesitated** (got there, but searched or
  backtracked) or **helped**.
- Afterwards, ask: "What was annoying?" and "What would you do in a
  calendar that you couldn't do here?"

## The task cards (hand these over)

Goals, not steps; the wording leaves the buttons for her to find.

1. Wat staat er deze week op de planning? Wanneer is de eerstvolgende
   afspraak met (iets dat echt in de agenda staat)?
2. Zet "Eten bij oma" in de gezinsagenda, zaterdag van 18:00 tot 21:00.
3. Het eten bij oma wordt zondag, zelfde tijd. Pas het aan.
4. Zet "Zwemles" in de agenda: elke woensdag om 16:00, een uur, tot de
   zomervakantie.
5. Volgende week gaat de zwemles niet door. Haal alleen die ene weg.
6. Je wilt een uur van tevoren herinnerd worden aan een afspraak in de
   agenda van je partner. Regel dat, alleen voor jezelf.
7. Verwijder het eten bij oma, en haal het daarna terug.
8. Zoek in je telefoon het telefoonnummer van (iemand uit de contacten).
   Voeg daarna een nieuw contact toe met een naam en een telefoonnummer.
9. Geef je eigen agenda een andere kleur.
10. Kijk samen op de telefoon van je partner of de zwemles daar ook
    staat.
11. Wanneer was (iets dat echt in de agenda stond, een paar maanden
    geleden)? Zoek het op.

English, for you: (1) what's on this week, find a real upcoming event;
(2) add dinner at grandma's, Saturday 18:00 to 21:00, family calendar;
(3) move it to Sunday; (4) a weekly swimming lesson until the summer
holiday; (5) skip only next week's; (6) a reminder for herself only on
your calendar; (7) delete and undo (Undo shows for 10 seconds only); (8)
find a phone number and add a contact, in the Contacts app; (9) recolor
her calendar (Settings, tap the calendar); (10) the series shows on your
phone; (11) find a past event (search).

Afterwards, check what she can't see:

- Task 6's reminder rings on her phone at the right time, and not on
  yours. Choose an event within the day, or set one up for this check.
- Task 10 arrived on your phone without a manual sync (note how long it
  took), and task 8's new contact is on your phone too if the address
  book is shared.
- Task 9's color shows on the web for her and not for you.

## Report back

Per task: the mark (alone, hesitated, helped), what she did, and what she
said. Add the app version (Settings, bottom). Screenshots help; crop or
blur real event titles and names, because I don't need the content, only
what happened. Add her answers to the two closing questions. Everything
she trips over becomes a fix, and the test passes when a rerun of the
stuck tasks goes without help.

## After it passes

1. Uninstall Fossify on both phones.
2. Use CalZone daily for a few weeks. Write down anything lost,
   duplicated, or ringing twice or not at all, with the date and time.
3. That closes phase 4b.
