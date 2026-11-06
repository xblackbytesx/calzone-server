-- Phase 2 step 6: ICS feeds both ways (decision 24).

-- Secret links that publish one calendar, full or busy-only. Only the
-- token's SHA-256 is stored; a revoked link is deleted.
CREATE TABLE feed_links (
    id            UUID PRIMARY KEY,
    collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    created_by    UUID REFERENCES principals (id) ON DELETE SET NULL,
    token_sha256  BYTEA NOT NULL UNIQUE,
    detail        TEXT NOT NULL CHECK (detail IN ('full', 'busy')),
    label         TEXT NOT NULL DEFAULT '',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at  TIMESTAMPTZ
);
CREATE INDEX feed_links_by_collection ON feed_links (collection_id);

-- A calendar filled from an outside .ics address. Read-only for everyone
-- (only the refresher writes); the last good copy stays until a newer one
-- parses completely.
CREATE TABLE subscriptions (
    collection_id    UUID PRIMARY KEY REFERENCES collections (id) ON DELETE CASCADE,
    url              TEXT NOT NULL,
    interval_minutes INTEGER NOT NULL DEFAULT 60 CHECK (interval_minutes BETWEEN 15 AND 10080),
    next_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_attempt_at  TIMESTAMPTZ,
    last_success_at  TIMESTAMPTZ,
    -- Why the last attempt failed, in plain words; never content.
    last_error       TEXT NOT NULL DEFAULT '',
    failures         INTEGER NOT NULL DEFAULT 0,
    etag             TEXT,
    last_modified    TEXT
);
CREATE INDEX subscriptions_due ON subscriptions (next_at);
