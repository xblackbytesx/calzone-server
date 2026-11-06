-- Two-factor sign-in for the web app (decision 32). DAV app passwords are
-- unaffected: they are random, per device and revocable.

ALTER TABLE principals
    -- AES-256-GCM (CALZONE_SECRET_KEY) of the TOTP secret: nonce || ciphertext.
    ADD COLUMN totp_secret BYTEA,
    -- The same, during setup, until a first code confirms it.
    ADD COLUMN totp_pending BYTEA,
    -- The last time step a code was accepted for: a code works once.
    ADD COLUMN totp_last_step BIGINT;

-- Single-use recovery codes, stored as SHA-256 (they are 50 random bits).
CREATE TABLE recovery_codes (
    principal_id UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    code_sha256  BYTEA NOT NULL,
    used_at      TIMESTAMPTZ,
    PRIMARY KEY (principal_id, code_sha256)
);

-- A session is unverified between the passphrase and the code; such a
-- session reaches nothing but the code page.
ALTER TABLE web_sessions ADD COLUMN verified BOOLEAN NOT NULL DEFAULT TRUE;
