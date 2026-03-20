-- Service-level environment variables (plaintext JSON MVP).
-- KMS envelope encryption deferred to post-MVP.
ALTER TABLE services ADD COLUMN IF NOT EXISTS env_vars JSONB NOT NULL DEFAULT '{}';
