-- Phase 2 step 2: personal settings, per-person calendar settings (decision
-- 8), and the history that powers undo in the web app (decision 12).

ALTER TABLE principals
    -- IANA zone; NULL until set (the calendar page sends the browser's).
    ADD COLUMN timezone TEXT,
    -- 'en' or 'nl'; NULL follows the browser (decision 35).
    ADD COLUMN language TEXT CHECK (language IN ('en', 'nl')),
    ADD COLUMN theme TEXT NOT NULL DEFAULT 'dark' CHECK (theme IN ('dark', 'light', 'system')),
    -- 1 = Monday ... 7 = Sunday.
    ADD COLUMN week_start SMALLINT NOT NULL DEFAULT 1 CHECK (week_start BETWEEN 1 AND 7),
    ADD COLUMN clock_24h BOOLEAN NOT NULL DEFAULT TRUE;

-- How one person sees one calendar: their own colour and name, whether it
-- shows, its place in their list, and their default reminder.
CREATE TABLE collection_prefs (
    principal_id  UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    color         TEXT,
    display_name  TEXT,
    hidden        BOOLEAN NOT NULL DEFAULT FALSE,
    sort_order    INTEGER,
    -- Minutes before the start; NULL: none.
    default_alarm INTEGER,
    PRIMARY KEY (principal_id, collection_id)
);

-- What a web write replaced, so its author can undo it. One change may
-- touch two objects (a "this and following" split). Kept 30 days.
CREATE TABLE object_history (
    id             BIGSERIAL PRIMARY KEY,
    change_id      UUID NOT NULL,
    principal_id   UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    collection_id  UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    -- The shared body before the change; NULL: it did not exist.
    before_body    TEXT,
    -- Every person's layer before the change: {principal id: layer}.
    before_layers  JSONB NOT NULL DEFAULT '{}',
    -- The revision the change left; NULL: it deleted the object.
    after_revision BIGINT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    undone_at      TIMESTAMPTZ
);
CREATE INDEX object_history_by_change ON object_history (change_id);
CREATE INDEX object_history_by_age ON object_history (created_at);
