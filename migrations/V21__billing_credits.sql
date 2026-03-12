-- Append-only credit ledger. Every balance change is a row for full auditability.
-- All amounts in micro-credits (i64): 1 micro-credit = $0.000001, $1 = 1,000,000.
CREATE TABLE credit_ledger (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id  UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    amount        BIGINT      NOT NULL,  -- positive = credit, negative = debit
    kind          TEXT        NOT NULL CHECK (kind IN (
        'subscription_credit',
        'topup',
        'usage_metal',
        'usage_liquid',
        'refund',
        'expiry'
    )),
    description   TEXT,
    reference_id  TEXT,         -- Stripe payment_intent ID, etc.
    balance_after BIGINT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_credit_ledger_workspace ON credit_ledger(workspace_id, created_at DESC);

-- Re-add balance tracking (was dropped in V17 during schema cleanup).
ALTER TABLE workspaces
    ADD COLUMN balance_credits BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN topup_credits   BIGINT NOT NULL DEFAULT 0;

-- Stripe customer lives on workspaces (not users) for billing purposes.
-- users.stripe_customer_id already exists but is user-level; workspace-level is
-- the correct billing entity since subscriptions are per-workspace.
ALTER TABLE workspaces
    ADD COLUMN stripe_customer_id TEXT UNIQUE;

-- Track billing period for monthly credit resets.
ALTER TABLE workspaces
    ADD COLUMN billing_period_start DATE,
    ADD COLUMN billing_period_end   DATE;
