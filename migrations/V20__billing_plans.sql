-- Tier definitions: limits and pricing.
-- Kept in a table (not hardcoded) so plan changes don't require a redeploy.
CREATE TABLE plans (
    id                   TEXT PRIMARY KEY,  -- 'hobby', 'pro', 'team'
    name                 TEXT    NOT NULL,
    price_cents          INT     NOT NULL DEFAULT 0,
    credit_cents         INT     NOT NULL DEFAULT 0,
    max_services         INT     NOT NULL,
    max_vcpu             INT     NOT NULL,
    max_memory_mb        INT     NOT NULL,
    allows_always_on     BOOLEAN NOT NULL DEFAULT false,
    max_wasm_invocations BIGINT  NOT NULL DEFAULT 0,  -- 0 = unlimited
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO plans (id, name, price_cents, credit_cents, max_services, max_vcpu, max_memory_mb, allows_always_on, max_wasm_invocations) VALUES
    ('hobby', 'Hobby',    0,    0, 2,  1,  128, false, 100000),
    ('pro',   'Pro',   1000, 1000, 10, 2,  512, true,  0),
    ('team',  'Team',  2000, 2000, 25, 4, 1024, true,  0);
