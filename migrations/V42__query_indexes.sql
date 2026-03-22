-- V42: Two index improvements from RBAC query optimization pass.

-- ── outbox: functional index on service_id payload field ─────────────────────
-- Used by stop_service and delete_service:
--   DELETE FROM outbox WHERE payload->>'service_id' = $1
-- Without this, every stop/delete does a full table scan on outbox.
CREATE INDEX IF NOT EXISTS idx_outbox_service_id
    ON outbox ((payload->>'service_id'));

-- ── domains: upgrade single-column index to composite ────────────────────────
-- verify_domain and remove_domain both filter WHERE service_id = $1 AND domain = $2.
-- The old single-column index found rows by service_id then checked domain as a
-- heap filter. The composite index satisfies the full predicate from the index.
-- The leading column (service_id) also covers the list_domains query, so the
-- old index is strictly subsumed.
DROP INDEX IF EXISTS idx_domains_service;
CREATE INDEX IF NOT EXISTS idx_domains_service_domain
    ON domains (service_id, domain);
