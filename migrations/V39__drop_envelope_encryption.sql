-- ─── V39: Drop envelope encryption columns ────────────────────────────────────
--
-- Secrets are now stored in HashiCorp Vault KV v2. The envelope encryption
-- columns in Postgres and the workspace_keys table are no longer needed.

-- Remove encrypted env var columns from services table.
ALTER TABLE services DROP COLUMN IF EXISTS env_ciphertext;
ALTER TABLE services DROP COLUMN IF EXISTS env_nonce;

-- Remove encrypted env var columns from project_env_vars (legacy, V6).
ALTER TABLE project_env_vars DROP COLUMN IF EXISTS value_ciphertext;
ALTER TABLE project_env_vars DROP COLUMN IF EXISTS value_nonce;

-- Remove encrypted cert PEM columns from domain_certs.
ALTER TABLE domain_certs DROP COLUMN IF EXISTS cert_pem_enc;
ALTER TABLE domain_certs DROP COLUMN IF EXISTS key_pem_enc;
ALTER TABLE domain_certs DROP COLUMN IF EXISTS cert_nonce;
ALTER TABLE domain_certs DROP COLUMN IF EXISTS key_nonce;

-- Drop workspace encryption keys table (DEKs no longer needed).
DROP TABLE IF EXISTS workspace_keys;
