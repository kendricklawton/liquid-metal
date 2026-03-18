-- Per-domain TLS certificates. Encrypted at rest with AES-GCM (CERT_ENCRYPTION_KEY).
-- nonce is prepended to ciphertext: [12 bytes nonce || ciphertext].
CREATE TABLE domain_certs (
    domain_id    UUID        PRIMARY KEY REFERENCES domains(id) ON DELETE CASCADE,
    cert_pem_enc BYTEA       NOT NULL,           -- AES-GCM encrypted PEM chain
    key_pem_enc  BYTEA       NOT NULL,           -- AES-GCM encrypted private key
    expires_at   TIMESTAMPTZ NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Short-lived ACME HTTP-01 challenge tokens. Pingora reads these to answer
-- /.well-known/acme-challenge/{token} requests during cert provisioning.
-- Rows are deleted by cert_manager after each provision attempt (success or failure).
CREATE TABLE acme_challenges (
    token             TEXT        PRIMARY KEY,
    key_authorization TEXT        NOT NULL,
    domain            TEXT        NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- GC stale challenges older than 10 minutes (missed cleanup, ACME timeout).
CREATE OR REPLACE FUNCTION gc_acme_challenges() RETURNS void LANGUAGE sql AS $$
    DELETE FROM acme_challenges WHERE created_at < NOW() - INTERVAL '10 minutes';
$$;
