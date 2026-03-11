-- V9: Add zitadel_sub to users for identity linkage
--
-- Stores the Zitadel subject claim (OIDC sub) so the API can
-- correlate CLI sessions with Zitadel identities.
-- Nullable: existing rows and provision calls without zitadel_sub are fine.

ALTER TABLE users ADD COLUMN IF NOT EXISTS zitadel_sub TEXT UNIQUE;

CREATE INDEX IF NOT EXISTS idx_users_zitadel_sub ON users(zitadel_sub) WHERE zitadel_sub IS NOT NULL;
