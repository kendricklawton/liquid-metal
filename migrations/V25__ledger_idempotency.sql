-- Prevent double-crediting from Stripe webhook retries.
-- The checkout session ID is unique per payment and serves as an idempotency key.
ALTER TABLE credit_ledger ADD COLUMN stripe_session_id TEXT;
CREATE UNIQUE INDEX idx_credit_ledger_stripe_session ON credit_ledger(stripe_session_id) WHERE stripe_session_id IS NOT NULL;
