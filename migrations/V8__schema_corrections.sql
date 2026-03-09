-- ============================================================================
-- V8: Schema corrections
--
-- 1. projects.slug: drop global unique constraint, add per-workspace unique.
--    Global uniqueness prevented two workspaces from having a project with
--    the same name (e.g. "backend"). Scoped to workspace is correct for a PaaS.
--
-- 2. users.tier: fix CHECK constraint to match actual tier values.
--    V1 used ('free', 'pro', 'enterprise'). Actual values are ('hobby', 'pro', 'team').
--    Note: workspaces.tier is the enforcement point — this is schema correctness.
--
-- 3. build_log_lines: add index on (service_id, created_at DESC) to support
--    partition pruning when the logs query includes a created_at filter.
-- ============================================================================

-- 1. Fix projects.slug uniqueness — scope to workspace
ALTER TABLE projects DROP CONSTRAINT IF EXISTS projects_slug_key;
ALTER TABLE projects ADD CONSTRAINT projects_slug_workspace_unique UNIQUE (workspace_id, slug);

-- 2. Fix users.tier CHECK constraint
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_tier_check;

-- Bring existing rows into compliance before re-adding the constraint
UPDATE users SET tier = 'hobby' WHERE tier = 'free';
UPDATE users SET tier = 'team'  WHERE tier = 'enterprise';

ALTER TABLE users ADD CONSTRAINT users_tier_check
    CHECK (tier IN ('hobby', 'pro', 'team'));

-- 3. Index for partition-aware logs queries
CREATE INDEX IF NOT EXISTS idx_build_logs_service_created
    ON build_log_lines(service_id, created_at DESC);
