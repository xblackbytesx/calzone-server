-- Contacts clean-up (decision 56): pairs of cards in one address book that
-- someone marked as different people, so the duplicate finder stops
-- suggesting them. Shared by everyone who can change that book; the card
-- names are the stored object names.
CREATE TABLE contact_not_duplicates (
    collection_id UUID NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    a             TEXT NOT NULL,
    b             TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (collection_id, a, b),
    CHECK (a < b)
);
