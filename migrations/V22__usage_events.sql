-- Fine-grained usage events. One row per metering tick (60s for Metal, batched for Liquid).
-- Aggregated periodically into credit deductions via the billing poller.
CREATE TABLE usage_events (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    service_id   UUID        NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    engine       TEXT        NOT NULL CHECK (engine IN ('metal', 'liquid')),
    quantity     BIGINT      NOT NULL,  -- Metal: seconds, Liquid: invocation count
    vcpu         INT         NOT NULL DEFAULT 0,
    memory_mb    INT         NOT NULL DEFAULT 0,
    billed       BOOLEAN     NOT NULL DEFAULT false,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_usage_events_unbilled
    ON usage_events(workspace_id, billed, created_at)
    WHERE billed = false;

CREATE INDEX idx_usage_events_service
    ON usage_events(service_id, created_at DESC);
