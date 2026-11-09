-- Phase 4 step 2: phones with the CalZone app (decision 10), their push
-- endpoints (decision 16), and the change notices that wake them.

-- One row per signed-in phone. The token is 256 random bits, stored only as
-- SHA-256; removing the row signs the phone out and stops its pushes.
CREATE TABLE devices (
    id           UUID PRIMARY KEY,
    principal_id UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    label        TEXT NOT NULL,
    token_sha256 BYTEA NOT NULL UNIQUE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    -- UnifiedPush: the endpoint (a capability, never logged) and the
    -- WebPush keys messages are encrypted to (RFC 8291).
    push_url     TEXT,
    push_key     TEXT,
    push_auth    TEXT,
    CHECK ((push_url IS NULL) = (push_key IS NULL) AND (push_url IS NULL) = (push_auth IS NULL))
);
CREATE INDEX devices_by_principal ON devices (principal_id);

-- One-time sign-in codes shown as a QR code on the Devices page.
CREATE TABLE device_links (
    code_sha256  BYTEA PRIMARY KEY,
    principal_id UUID NOT NULL REFERENCES principals (id) ON DELETE CASCADE,
    expires_at   TIMESTAMPTZ NOT NULL,
    used_at      TIMESTAMPTZ
);

-- Change notices for push. Postgres delivers them at commit (and once per
-- distinct payload per transaction), so every door's writes wake the phones
-- without the doors knowing about push. Payload: a collection id, or
-- `<collection id>/<principal id>` when only that person's view changed.
-- A new or changed grant: the people who can see it now hear about it (a
-- removed one is noticed at the phone's next sync, which lists collections).
CREATE FUNCTION calzone_notify_grant() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('calzone_changes', NEW.collection_id::text);
    RETURN NEW;
END $$;

CREATE FUNCTION calzone_notify_collection_row() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.sequence IS DISTINCT FROM OLD.sequence
       OR NEW.display_name IS DISTINCT FROM OLD.display_name
       OR NEW.description IS DISTINCT FROM OLD.description
       OR NEW.color IS DISTINCT FROM OLD.color THEN
        PERFORM pg_notify('calzone_changes', NEW.id::text);
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION calzone_notify_prefs() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('calzone_changes', NEW.collection_id::text || '/' || NEW.principal_id::text);
    RETURN NEW;
END $$;

CREATE TRIGGER collections_notify AFTER UPDATE ON collections
    FOR EACH ROW EXECUTE FUNCTION calzone_notify_collection_row();
CREATE TRIGGER grants_notify AFTER INSERT OR UPDATE ON grants
    FOR EACH ROW EXECUTE FUNCTION calzone_notify_grant();
CREATE TRIGGER collection_prefs_notify AFTER INSERT OR UPDATE ON collection_prefs
    FOR EACH ROW EXECUTE FUNCTION calzone_notify_prefs();
