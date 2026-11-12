-- The Black theme for OLED screens (decision 55), next to dark, light and
-- system. Only the allowed values change.
ALTER TABLE principals DROP CONSTRAINT principals_theme_check;
ALTER TABLE principals ADD CONSTRAINT principals_theme_check
    CHECK (theme IN ('dark', 'black', 'light', 'system'));
