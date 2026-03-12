-- Node capability registry.
-- Each daemon upserts its row on startup and every 60s.
-- Rows older than 2 minutes are considered dead — the node has crashed or been deprovisioned.
-- The API capacity gate sums capacity_mb from live rows to determine how much
-- Metal memory is available cluster-wide, without any hardcoded values.
CREATE TABLE node_heartbeats (
    node_id     TEXT        PRIMARY KEY,
    engine      TEXT        NOT NULL CHECK (engine IN ('metal', 'liquid')),
    capacity_mb INT         NOT NULL,   -- usable RAM this node can allocate (MB)
    last_seen   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
