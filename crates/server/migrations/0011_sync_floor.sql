-- Phase 4 step 3: after a restore from a backup, `calzone-server resync`
-- moves every collection's change log past a gap and records where the
-- new numbering starts. A sync state below it was issued before the
-- restore and may name changes the server no longer has, so it is refused
-- and the phone (or DAV client) syncs from the start (decision 48).
ALTER TABLE collections ADD COLUMN floor BIGINT NOT NULL DEFAULT 0;
