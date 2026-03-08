-- ─── V6: Encrypted environment variables ─────────────────────────────────────
--
-- Adds AES-256-GCM encrypted columns to `project_env_vars`.
-- The plaintext `value` column is kept during the transition period; it should
-- be set to NULL (or the column dropped) after all rows have been re-encrypted
-- and the application is serving exclusively from encrypted storage.
--
-- Encryption scheme:
--   1. Resolve the workspace's active DEK from `workspace_keys`.
--   2. Unwrap the DEK via KMS (see envelope.rs / GoogleKmsClient).
--   3. AES-256-GCM encrypt value → store (value_ciphertext, value_nonce).
--   4. On read: unwrap DEK, decrypt ciphertext with stored nonce.

ALTER TABLE project_env_vars
  ADD COLUMN IF NOT EXISTS value_ciphertext BYTEA,
  ADD COLUMN IF NOT EXISTS value_nonce      BYTEA;

COMMENT ON COLUMN project_env_vars.value_ciphertext IS
  'AES-256-GCM ciphertext of the env var value. NULL = not yet migrated to encryption.';
COMMENT ON COLUMN project_env_vars.value_nonce IS
  'AES-256-GCM nonce (12 bytes) paired with value_ciphertext.';

-- Once all rows are migrated and the application only writes encrypted values,
-- run the following to drop the plaintext column:
--   ALTER TABLE project_env_vars DROP COLUMN value;
-- Do NOT run this in this migration — it is an intentional out-of-band step
-- performed after verifying all data is readable from encrypted storage.
