-- Deployment history for rollback support.
-- Each deploy (including redeploys) inserts a row here.
CREATE TABLE IF NOT EXISTS deployments (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id   UUID        NOT NULL,
    workspace_id UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    slug         TEXT        NOT NULL,
    engine       TEXT        NOT NULL,
    artifact_key TEXT        NOT NULL,
    commit_sha   TEXT,
    port         INT,
    env_vars     JSONB       NOT NULL DEFAULT '{}',
    run_mode     TEXT        NOT NULL DEFAULT 'serverless',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deployed_by  UUID        REFERENCES users(id)
);

CREATE INDEX IF NOT EXISTS idx_deployments_slug ON deployments(slug, created_at DESC);
