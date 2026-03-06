-- V3: node tracking columns on services
-- node_id: identifies which bare-metal node hosts this service (for HA + deprovision routing)
-- tap_name: the TAP device name for Metal VMs (e.g. tap0) — persisted for post-restart recovery

ALTER TABLE services
    ADD COLUMN IF NOT EXISTS node_id  TEXT,
    ADD COLUMN IF NOT EXISTS tap_name TEXT;

-- Index for daemon startup query: "how many Metal VMs is this node running?"
CREATE INDEX IF NOT EXISTS idx_services_node_engine
    ON services (node_id, engine, status)
    WHERE deleted_at IS NULL;
