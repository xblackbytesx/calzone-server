-- Decision 57: a Birthdays calendar from contacts.

-- A person's Birthdays calendar: an ordinary calendar, filled only by the
-- server from the address books the person reads (and those shared with
-- them for birthdays only). Read-only for everyone; never shared.
CREATE TABLE birthday_calendars (
    collection_id UUID PRIMARY KEY REFERENCES collections (id) ON DELETE CASCADE,
    principal_id  UUID NOT NULL UNIQUE REFERENCES principals (id) ON DELETE CASCADE,
    anniversaries BOOLEAN NOT NULL DEFAULT FALSE,
    -- The person's reminder on every event, minutes before the day starts
    -- (-540 is 9:00 on the day); NULL for none.
    reminder      INTEGER CHECK (reminder BETWEEN -1440 AND 40320),
    -- Titles' language when the person follows the browser.
    language      TEXT NOT NULL DEFAULT 'en' CHECK (language IN ('en', 'nl')),
    -- What the last rebuild was made from; anything else now rebuilds.
    built_from    TEXT NOT NULL DEFAULT ''
);

-- Address books a person left out of their Birthdays.
CREATE TABLE birthday_skips (
    principal_id  UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    PRIMARY KEY (principal_id, collection_id)
);

-- Birthdays-only sharing: this address book's names and dates go to this
-- person's (or this group's members') Birthdays calendar. Never the
-- cards: no access check reads this table, so it opens nothing else.
CREATE TABLE birthday_shares (
    collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    principal_id  UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (collection_id, principal_id)
);
