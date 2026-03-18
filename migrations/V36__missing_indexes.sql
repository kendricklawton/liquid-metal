-- V36: Add missing indexes and fix domains.deleted_at column.
--
-- Identified by cross-referencing every SQL query in the codebase against
-- existing indexes. Also fixes a bug: cert_manager.rs references
-- domains.deleted_at but the column didn't exist.

-- ── Fix: domains table missing deleted_at ────────────────────────────────────
-- Every other core entity (users, workspaces, projects, services) has a
-- deleted_at soft-delete column. The domains table was missing it, causing
-- cert_manager queries to fail with "column d.deleted_at does not exist".
ALTER TABLE domains ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

-- ── deployments(service_id, created_at DESC) ─────────────────────────────────
-- Used by: list deployments for a service, rollback to previous deployment.
-- Without this, every deployment list/rollback does a sequential scan.
CREATE INDEX IF NOT EXISTS idx_deployments_service
    ON deployments(service_id, created_at DESC);

-- ── usage_events(workspace_id, engine, created_at) ───────────────────────────
-- Used by: billing dashboard queries that aggregate Metal/Liquid usage for a
-- workspace within a billing period. The existing idx_usage_events_unbilled
-- only covers billed=false scenarios; billing period queries filter on
-- workspace_id + engine + date range.
CREATE INDEX IF NOT EXISTS idx_usage_events_workspace_engine
    ON usage_events(workspace_id, engine, created_at);

-- ── Covering index for proxy cache warm / reconciler ─────────────────────────
-- Runs on proxy startup and every 60s. Returns (slug, upstream_addr) for all
-- running services. This covering index lets Postgres do an index-only scan
-- instead of hitting the heap for every row.
CREATE INDEX IF NOT EXISTS idx_services_running
    ON services(slug, upstream_addr)
    WHERE status = 'running' AND upstream_addr IS NOT NULL AND deleted_at IS NULL;
