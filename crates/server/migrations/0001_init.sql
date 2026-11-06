-- Phase 1 step 1: the instance record. Step 3 adds principals, collections,
-- grants, objects, user layers and the change log in its own migration.
CREATE TABLE instance (
    id         BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO instance DEFAULT VALUES;
