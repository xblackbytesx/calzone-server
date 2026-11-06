-- Phase 1 step 5: web logins (decision 10, decision 29).

ALTER TABLE principals
    -- Argon2id PHC string; NULL until an admin issues a temporary passphrase.
    ADD COLUMN passphrase_hash TEXT,
    -- Admins manage people and groups in the web app.
    ADD COLUMN is_admin BOOLEAN NOT NULL DEFAULT FALSE;

-- Browser sessions: the cookie is 256 random bits, stored only as SHA-256.
CREATE TABLE web_sessions (
    token_sha256 BYTEA PRIMARY KEY,
    principal_id UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    expires_at   TIMESTAMPTZ NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX web_sessions_by_principal ON web_sessions (principal_id);
