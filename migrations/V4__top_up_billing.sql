-- ============================================================================
-- V4: Top-Up Billing & Daemon Heartbeats
--
-- 1. Adds micro-credit balance to workspaces.
-- 2. Adds high-velocity usage_pulses table for the Node Daemon to report to.
-- 3. Adds last_heartbeat_at to services for the Reconciler to detect dead VMs.
-- ============================================================================

-- 1. Workspace Balance (1 credit = $0.000001 to avoid float math)
ALTER TABLE workspaces
    ADD COLUMN balance_credits BIGINT NOT NULL DEFAULT 0;

COMMENT ON COLUMN workspaces.balance_credits IS 'Balance in micro-credits. $1.00 = 1,000,000';

-- 2. Service Heartbeat Tracking
ALTER TABLE services
    ADD COLUMN last_heartbeat_at TIMESTAMPTZ;

-- 3. High-Velocity Usage Pulses
CREATE TABLE usage_pulses (
    id           UUID        PRIMARY KEY,
    service_id   UUID        NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    workspace_id UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    duration_ms  BIGINT      NOT NULL,
    vcpu         INT         NOT NULL,
    memory_mb    INT         NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index to help the API quickly aggregate un-billed pulses per workspace
CREATE INDEX idx_usage_pulses_workspace ON usage_pulses(workspace_id, created_at);
