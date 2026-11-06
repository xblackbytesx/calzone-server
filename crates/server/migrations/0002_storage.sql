-- Phase 1 step 3: principals, collections, grants, objects, per-user layers
-- and the change log (AGENTS.md decisions 5 to 8). Credentials come with
-- step 4 (the DAV door's app passwords) and step 5 (web passphrases).

CREATE TABLE principals (
    id           UUID PRIMARY KEY,
    kind         TEXT NOT NULL CHECK (kind IN ('user', 'group')),
    -- URL segment and login name: a-z 0-9 . _ -, unique case-insensitively.
    name         TEXT NOT NULL UNIQUE CHECK (name = lower(name)),
    display_name TEXT NOT NULL DEFAULT '',
    disabled     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE memberships (
    group_id  UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    member_id UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, member_id)
);

CREATE TABLE collections (
    id           UUID PRIMARY KEY,
    owner_id     UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('calendar', 'addressbook')),
    slug         TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    description  TEXT NOT NULL DEFAULT '',
    color        TEXT,
    -- Calendars: the component types they accept (VEVENT, VTODO, VJOURNAL).
    components   TEXT[] NOT NULL DEFAULT '{}',
    -- The change log's head: bumped in the same transaction as every write
    -- and delete. Sync tokens, CTags and ETags all derive from it (decision 6).
    sequence     BIGINT NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (owner_id, slug)
);

CREATE TABLE grants (
    collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    principal_id  UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    level         TEXT NOT NULL CHECK (level IN ('read', 'write', 'admin')),
    PRIMARY KEY (collection_id, principal_id)
);

CREATE TABLE objects (
    id            UUID PRIMARY KEY,
    collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    -- The resource name in its collection (`abc.ics`).
    name          TEXT NOT NULL,
    uid           TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('VEVENT', 'VTODO', 'VJOURNAL', 'VCARD')),
    -- The shared object exactly as written, minus every user's personal items.
    body          TEXT NOT NULL,
    -- The collection sequence of the last write: the ETag.
    revision      BIGINT NOT NULL,
    -- Projection (decision 5). Calendars: UTC bounds, NULL end = open-ended,
    -- undated = matches every range. Cards: the formatted name and emails.
    undated       BOOLEAN NOT NULL DEFAULT FALSE,
    starts_at     TIMESTAMP,
    ends_at       TIMESTAMP,
    recurring     BOOLEAN NOT NULL DEFAULT FALSE,
    summary       TEXT,
    emails        TEXT[] NOT NULL DEFAULT '{}',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (collection_id, name),
    UNIQUE (collection_id, uid)
);
CREATE INDEX objects_span ON objects (collection_id, starts_at, ends_at);

-- One user's personal items for one object (decision 8): reminders and the
-- client state around them, as core::split::UserLayer JSON.
CREATE TABLE user_layers (
    object_id    UUID NOT NULL REFERENCES objects (id) ON DELETE CASCADE,
    principal_id UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    layer        JSONB NOT NULL,
    PRIMARY KEY (object_id, principal_id)
);

-- Every write and delete, in order. Deletes stay as tombstones so a client
-- syncing from an old token learns what went away.
CREATE TABLE changes (
    collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    sequence      BIGINT NOT NULL,
    name          TEXT NOT NULL,
    deleted       BOOLEAN NOT NULL,
    PRIMARY KEY (collection_id, sequence)
);
CREATE INDEX changes_by_name ON changes (collection_id, name, sequence DESC);
