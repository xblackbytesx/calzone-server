-- Phase 1 step 4: app passwords for DAV clients (decision 10, decision 28).

CREATE TABLE app_passwords (
    id           UUID PRIMARY KEY,
    principal_id UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    label        TEXT NOT NULL,
    -- SHA-256 (hex) of the normalised password. The password is 125 random
    -- bits, so a slow hash adds nothing and every DAV request stays cheap.
    hash         TEXT NOT NULL UNIQUE,
    -- FALSE: everything the user can reach. TRUE: only the scope rows below.
    scoped       BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ
);
CREATE INDEX app_passwords_by_principal ON app_passwords (principal_id);

CREATE TABLE app_password_scopes (
    password_id   UUID NOT NULL REFERENCES app_passwords (id) ON DELETE CASCADE,
    collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    level         TEXT NOT NULL CHECK (level IN ('read', 'write')),
    PRIMARY KEY (password_id, collection_id)
);
