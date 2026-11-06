-- "Keep me signed in on this device" (decision 33): a persistent session
-- lasts 30 days, any other 12 hours with a browser-session cookie. Pending
-- (two-factor) sessions carry the choice to the session that follows.
ALTER TABLE web_sessions ADD COLUMN persistent BOOLEAN NOT NULL DEFAULT TRUE;
