-- V13: Add artifact_key to services for restart support
--
-- The deploy handler stores the artifact_key so the daemon can re-download
-- the artifact on restart without requiring a full redeploy.

ALTER TABLE services ADD COLUMN IF NOT EXISTS artifact_key TEXT;
