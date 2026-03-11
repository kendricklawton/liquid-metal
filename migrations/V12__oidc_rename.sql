-- V12: rename zitadel_sub → oidc_sub for provider-agnostic OIDC identity linkage
--
-- The sub claim is a standard OIDC concept, not Zitadel-specific.
-- Renaming allows swapping auth providers (Auth0, Keycloak, etc.) without schema changes.

ALTER TABLE users RENAME COLUMN zitadel_sub TO oidc_sub;

DROP INDEX IF EXISTS idx_users_zitadel_sub;
CREATE INDEX IF NOT EXISTS idx_users_oidc_sub ON users(oidc_sub) WHERE oidc_sub IS NOT NULL;
