-- ─── V40: Per-service billing model ─────────────────────────────────────────
--
-- Replaces workspace-level tier subscriptions (hobby/pro/team) with:
--   Metal: fixed monthly price per service (1/2/4 vCPU tiers)
--   Liquid: per-invocation billing ($0.30/1M, 1M free/mo per workspace)
--
-- No workspace tiers. No subscriptions. Top-up credits only.

-- Drop the plans table (hobby/pro/team no longer exists).
DROP TABLE IF EXISTS plans;

-- Remove workspace-level tier/subscription columns.
ALTER TABLE workspaces DROP COLUMN IF EXISTS tier;
ALTER TABLE workspaces DROP COLUMN IF EXISTS billing_period_start;
ALTER TABLE workspaces DROP COLUMN IF EXISTS billing_period_end;
ALTER TABLE workspaces DROP COLUMN IF EXISTS stripe_subscription_id;

-- Merge balance_credits + topup_credits into single balance pool.
ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS balance BIGINT NOT NULL DEFAULT 0;
UPDATE workspaces SET balance = COALESCE(balance_credits, 0) + COALESCE(topup_credits, 0)
  WHERE balance = 0;
ALTER TABLE workspaces DROP COLUMN IF EXISTS balance_credits;
ALTER TABLE workspaces DROP COLUMN IF EXISTS topup_credits;

-- Track free Liquid invocations per workspace per month.
ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS free_invocations_used BIGINT NOT NULL DEFAULT 0;
ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS free_invocations_reset_at
  TIMESTAMPTZ NOT NULL DEFAULT date_trunc('month', NOW());

-- Metal tier on services (null for Liquid).
ALTER TABLE services ADD COLUMN IF NOT EXISTS metal_tier TEXT
  CHECK (metal_tier IN ('one', 'two', 'four'));
ALTER TABLE services ADD COLUMN IF NOT EXISTS monthly_price_cents INTEGER;
ALTER TABLE services ADD COLUMN IF NOT EXISTS billing_cycle_start TIMESTAMPTZ;

-- Remove legacy tier from users table.
ALTER TABLE users DROP COLUMN IF EXISTS tier;
