-- ─── V32: Encrypted service environment variables ────────────────────────────
--
-- Adds envelope-encrypted columns to the services table. The existing plaintext
-- `env_vars` JSONB column is retained during the transition period.
--
-- After all services have been re-encrypted and the API is serving exclusively
-- from encrypted storage, drop the plaintext column:
--   ALTER TABLE services DROP COLUMN env_vars;

ALTER TABLE services
  ADD COLUMN IF NOT EXISTS env_ciphertext BYTEA,
  ADD COLUMN IF NOT EXISTS env_nonce      BYTEA;

COMMENT ON COLUMN services.env_ciphertext IS
  'AES-256-GCM ciphertext of the env vars JSON blob. NULL = no env vars set.';
COMMENT ON COLUMN services.env_nonce IS
  'AES-256-GCM nonce (12 bytes) paired with env_ciphertext.';
