-- Fix credit_ledger kind CHECK constraint.
--
-- V21 created: ('subscription_credit', 'topup', 'usage_metal', 'usage_liquid', 'refund', 'expiry')
-- Code writes 'metal_monthly' (billing.rs, deployments.rs) which violates the CHECK.
-- 'subscription_credit' and 'usage_metal' are dead kinds — nothing writes them since
-- V40 dropped the subscription/plans model.
ALTER TABLE credit_ledger DROP CONSTRAINT credit_ledger_kind_check;
ALTER TABLE credit_ledger ADD CONSTRAINT credit_ledger_kind_check
    CHECK (kind IN ('topup', 'usage_liquid', 'metal_monthly', 'refund', 'expiry'));
