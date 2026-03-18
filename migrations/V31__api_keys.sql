-- Scoped API keys with hashed tokens.
-- Users can create multiple keys with different scopes and expiry dates.
-- The token is shown once at creation; only the SHA-256 hash is stored.

CREATE TABLE api_keys (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id       UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name          TEXT        NOT NULL,
    -- SHA-256 hash of the token (hex-encoded, 64 chars).
    -- The raw token (lm_...) is returned once at creation and never stored.
    token_hash    TEXT        NOT NULL UNIQUE,
    -- Prefix of the raw token for display (e.g. "lm_abc1..."). Never enough to reconstruct.
    token_prefix  TEXT        NOT NULL,
    -- Scopes: 'read', 'write', 'admin'. Stored as postgres text array.
    -- read  = GET endpoints (list, status, logs, balance)
    -- write = read + mutate (deploy, stop, restart, scale, env, domains)
    -- admin = write + destructive (delete, billing topup/subscribe, workspace delete)
    scopes        TEXT[]      NOT NULL DEFAULT '{read}',
    expires_at    TIMESTAMPTZ,
    last_used_at  TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at    TIMESTAMPTZ
);

CREATE INDEX idx_api_keys_token_hash ON api_keys (token_hash) WHERE deleted_at IS NULL;
CREATE INDEX idx_api_keys_user_id ON api_keys (user_id) WHERE deleted_at IS NULL;
