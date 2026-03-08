-- ─── V5: Security hardening ───────────────────────────────────────────────────
--
-- 1. Creates a restricted `lm_app` database role with DML-only permissions.
--    In production:
--      a. CREATE USER lm_app_user WITH PASSWORD '<strong-secret>';
--      b. GRANT lm_app TO lm_app_user;
--      c. Set DATABASE_URL to connect as lm_app_user.
--      d. Set MIGRATIONS_DATABASE_URL to connect as the database owner.
--    This ensures the running application can never execute DDL (ALTER TABLE,
--    DROP TABLE, CREATE INDEX, etc.), limiting blast radius if it is compromised.
--
-- 2. Creates the `workspace_keys` table for KMS envelope encryption.
--    Each workspace has one active DEK, wrapped by the KMS CMK.

-- ── App role ──────────────────────────────────────────────────────────────────

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'lm_app') THEN
    CREATE ROLE lm_app;
  END IF;
END
$$;

-- Revoke any previously granted privileges, then re-grant DML only.
REVOKE ALL PRIVILEGES ON ALL TABLES    IN SCHEMA public FROM lm_app;
REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public FROM lm_app;

GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES    IN SCHEMA public TO lm_app;
GRANT USAGE, SELECT                  ON ALL SEQUENCES IN SCHEMA public TO lm_app;

-- Ensure future objects (created by migrations) are also accessible.
ALTER DEFAULT PRIVILEGES IN SCHEMA public
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES    TO lm_app;
ALTER DEFAULT PRIVILEGES IN SCHEMA public
  GRANT USAGE, SELECT                  ON SEQUENCES TO lm_app;

-- ── Workspace encryption keys ─────────────────────────────────────────────────
--
-- Stores KMS-wrapped Data Encryption Keys (DEKs) for per-workspace envelope
-- encryption of secrets (project_env_vars.value_ciphertext).
--
-- Workflow:
--   Write: generate DEK → KMS.wrap(DEK) → store encrypted_dek here
--          → AES-256-GCM encrypt value with plaintext DEK → store in project_env_vars
--   Read:  fetch encrypted_dek → KMS.unwrap(encrypted_dek) → AES-256-GCM decrypt value

CREATE TABLE workspace_keys (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id    UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    encrypted_dek   BYTEA       NOT NULL,       -- KMS-wrapped DEK ciphertext
    kms_key_version TEXT        NOT NULL,       -- KMS key version URI used for wrapping
    algorithm       TEXT        NOT NULL DEFAULT 'AES256GCM',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    active          BOOLEAN     NOT NULL DEFAULT TRUE
);

-- Enforce one active DEK per workspace (partial unique index).
CREATE UNIQUE INDEX idx_workspace_keys_one_active
  ON workspace_keys (workspace_id)
  WHERE active = TRUE;

COMMENT ON TABLE workspace_keys IS
  'KMS-wrapped Data Encryption Keys for per-workspace envelope encryption.';
COMMENT ON COLUMN workspace_keys.encrypted_dek IS
  'The DEK encrypted (wrapped) by the KMS CMK. Never store the plaintext DEK.';
COMMENT ON COLUMN workspace_keys.kms_key_version IS
  'Full KMS key version resource name used to wrap this DEK (enables rotation tracking).';
