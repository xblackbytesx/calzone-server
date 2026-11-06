-- Phase 1 step 6: import reports (decision 30). Credentials for a Baikal
-- import are never stored; only what happened.

CREATE TABLE imports (
    id           UUID PRIMARY KEY,
    -- The account the objects went into (alarms land in its layer).
    principal_id UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    -- Who started it (an admin may import for someone else).
    started_by   UUID REFERENCES principals (id) ON DELETE SET NULL,
    -- "file family.ics" or "Baikal https://host/dav.php/"; never a password.
    source       TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('running', 'done', 'failed', 'interrupted')),
    -- Why it failed as a whole (status 'failed'); per-object failures are rows below.
    message      TEXT NOT NULL DEFAULT '',
    -- Per collection: name, id, whether it was created, and the counts.
    summary      JSONB NOT NULL DEFAULT '[]',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at  TIMESTAMPTZ
);
CREATE INDEX imports_by_principal ON imports (principal_id, created_at DESC);
CREATE INDEX imports_by_starter ON imports (started_by, created_at DESC);

-- Objects that could not be stored, kept as the exact bytes received so
-- nothing is ever dropped silently.
CREATE TABLE import_failures (
    import_id  UUID NOT NULL REFERENCES imports (id) ON DELETE CASCADE,
    position   INTEGER NOT NULL,
    collection TEXT NOT NULL,
    reason     TEXT NOT NULL,
    raw        BYTEA NOT NULL,
    PRIMARY KEY (import_id, position)
);
