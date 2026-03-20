
-- V1: Platform schema — Liquid Metal PaaS
-- Ported from project-platform/core/migrations (0001–0003)
--
-- PK strategy : UUIDv7 (application-generated, time-sortable)
-- Encryption  : Envelope encryption via KMS for secrets
-- Partitioning: build_log_lines, audit_log (monthly by created_at)

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- UTILITY: updated_at trigger
CREATE OR REPLACE FUNCTION set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- USERS
CREATE TABLE users (
    id                 UUID        PRIMARY KEY,
    email              TEXT        UNIQUE NOT NULL,
    name               TEXT        NOT NULL,
    avatar_url         TEXT,
    stripe_customer_id TEXT        UNIQUE,
    tier               TEXT        NOT NULL DEFAULT 'free'
                                   CHECK (tier IN ('free', 'pro', 'enterprise')),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at         TIMESTAMPTZ
);

CREATE INDEX idx_users_active ON users(email) WHERE deleted_at IS NULL;

CREATE TRIGGER trg_users_updated_at
    BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- WORKSPACES (formerly "teams" — renamed in 0003_rename_teams_to_workspaces)
CREATE TABLE workspaces (
    id                       UUID        PRIMARY KEY,
    name                     TEXT        NOT NULL,
    slug                     TEXT        UNIQUE NOT NULL,
    stripe_subscription_id   TEXT        UNIQUE,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at               TIMESTAMPTZ
);

CREATE INDEX idx_workspaces_active ON workspaces(slug) WHERE deleted_at IS NULL;

CREATE TRIGGER trg_workspaces_updated_at
    BEFORE UPDATE ON workspaces
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- WORKSPACE MEMBERS (RBAC: owner, member, viewer)
CREATE TABLE workspace_members (
    workspace_id UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id      UUID        NOT NULL REFERENCES users(id)      ON DELETE CASCADE,
    role         TEXT        NOT NULL CHECK (role IN ('owner', 'member', 'viewer')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (workspace_id, user_id)
);

CREATE INDEX idx_workspace_members_user ON workspace_members(user_id);

-- PROJECTS
-- A project is a Git repo + build config. One project → many services.
CREATE TABLE projects (
    id              UUID        PRIMARY KEY,
    workspace_id    UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name            TEXT        NOT NULL,
    slug            TEXT        UNIQUE NOT NULL,
    repo_url        TEXT        NOT NULL,
    default_branch  TEXT        NOT NULL DEFAULT 'main',
    root_directory  TEXT        NOT NULL DEFAULT './',
    build_command   TEXT,
    install_command TEXT,
    output_directory TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at      TIMESTAMPTZ,
    UNIQUE(workspace_id, name)
);

CREATE INDEX idx_projects_workspace ON projects(workspace_id, created_at DESC);

CREATE TRIGGER trg_projects_updated_at
    BEFORE UPDATE ON projects
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- DOMAINS
CREATE TABLE domains (
    id                UUID        PRIMARY KEY,
    project_id        UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    domain            TEXT        UNIQUE NOT NULL,
    is_verified       BOOLEAN     NOT NULL DEFAULT false,
    verification_type TEXT        NOT NULL DEFAULT 'cname'
                                  CHECK (verification_type IN ('cname', 'txt')),
    verification_token TEXT       NOT NULL DEFAULT encode(gen_random_bytes(32), 'hex'),
    tls_status        TEXT        NOT NULL DEFAULT 'pending'
                                  CHECK (tls_status IN ('pending', 'provisioning', 'active', 'error')),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_domains_project ON domains(project_id);

CREATE TRIGGER trg_domains_updated_at
    BEFORE UPDATE ON domains
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- SECRETS / ENV VARS (Envelope encryption)
CREATE TABLE project_env_vars (
    id                  UUID        PRIMARY KEY,
    project_id          UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    environment         TEXT        NOT NULL CHECK (environment IN ('production', 'preview', 'development')),
    key_name            TEXT        NOT NULL,
    encrypted_value     BYTEA       NOT NULL,
    encrypted_data_key  BYTEA       NOT NULL,
    encryption_iv       BYTEA       NOT NULL,
    kms_key_id          TEXT        NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(project_id, environment, key_name)
);

CREATE INDEX idx_env_vars_project_env ON project_env_vars(project_id, environment);

CREATE TRIGGER trg_env_vars_updated_at
    BEFORE UPDATE ON project_env_vars
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- WEBHOOKS (GitHub / GitLab / Bitbucket)
CREATE TABLE webhooks (
    id                      UUID        PRIMARY KEY,
    project_id              UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    provider                TEXT        NOT NULL CHECK (provider IN ('github', 'gitlab', 'bitbucket')),
    provider_install_id     TEXT        NOT NULL,
    hook_secret_encrypted   BYTEA       NOT NULL,
    events                  TEXT[]      NOT NULL DEFAULT '{push}',
    is_active               BOOLEAN     NOT NULL DEFAULT true,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_webhooks_project ON webhooks(project_id);
CREATE UNIQUE INDEX idx_webhooks_provider_install ON webhooks(provider, provider_install_id);

CREATE TRIGGER trg_webhooks_updated_at
    BEFORE UPDATE ON webhooks
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- SERVICES
--
-- Replaces "deployments" from project-platform. Each service is a live,
-- running compute unit — either a Firecracker microVM (metal) or a Wasmtime
-- executor (liquid). Linked to a project; one project can have multiple
-- services (e.g. production + preview).
--
-- slug  : Pingora hot-path lookup on every request — must stay fast.
-- upstream_addr : written by daemon after the VM/Wasm is ready.
CREATE TABLE services (
    id           UUID        PRIMARY KEY,
    project_id   UUID        REFERENCES projects(id) ON DELETE SET NULL,
    workspace_id UUID        REFERENCES workspaces(id) ON DELETE CASCADE,
    name         TEXT        NOT NULL,
    slug         TEXT        UNIQUE NOT NULL,
    engine       TEXT        NOT NULL CHECK (engine IN ('metal', 'liquid')),

    -- Git context (set on deploy, null for manually created services)
    branch       TEXT,
    commit_sha   TEXT,
    commit_message TEXT,

    -- Metal engine config
    vcpu         INT,
    memory_mb    INT,
    port         INT,
    rootfs_path  TEXT,

    -- liquid engine config
    wasm_path    TEXT,

    -- Runtime state — written by daemon after boot
    status       TEXT        NOT NULL DEFAULT 'provisioning'
                             CHECK (status IN ('provisioning', 'running', 'stopped', 'error', 'canceled')),
    upstream_addr TEXT,

    -- Timing
    provisioned_at  TIMESTAMPTZ,
    stopped_at      TIMESTAMPTZ,

    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at   TIMESTAMPTZ
);

-- Pingora hot path: slug lookup on every inbound request
CREATE INDEX idx_services_slug      ON services(slug)                          WHERE deleted_at IS NULL;
CREATE INDEX idx_services_workspace ON services(workspace_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_services_project   ON services(project_id,   created_at DESC);
-- Active work queue (daemon polling for stuck provisioning)
CREATE INDEX idx_services_provisioning ON services(status, created_at)
    WHERE status IN ('provisioning') AND deleted_at IS NULL;

CREATE TRIGGER trg_services_updated_at
    BEFORE UPDATE ON services
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- BUILD LOG LINES (Partitioned by month)
--
-- Linked to services (replaces "deployment_id" from project-platform).
-- PK is (id, created_at) — Postgres requires partition key in PK.
CREATE TABLE build_log_lines (
    id          UUID        NOT NULL,
    service_id  UUID        NOT NULL,
    line_number INT         NOT NULL,
    content     TEXT        NOT NULL,
    stream      TEXT        NOT NULL DEFAULT 'stdout' CHECK (stream IN ('stdout', 'stderr')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

-- FK enforced at application layer (partitioned table limitation pre-PG17)
CREATE INDEX idx_build_logs_service ON build_log_lines(service_id, line_number);

CREATE TABLE build_log_lines_2026_03 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-03-01') TO ('2026-04-01');
CREATE TABLE build_log_lines_2026_04 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-04-01') TO ('2026-05-01');
CREATE TABLE build_log_lines_2026_05 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-05-01') TO ('2026-06-01');
CREATE TABLE build_log_lines_2026_06 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-06-01') TO ('2026-07-01');
CREATE TABLE build_log_lines_2026_07 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-07-01') TO ('2026-08-01');
CREATE TABLE build_log_lines_2026_08 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-08-01') TO ('2026-09-01');
CREATE TABLE build_log_lines_2026_09 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-09-01') TO ('2026-10-01');
CREATE TABLE build_log_lines_2026_10 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');
CREATE TABLE build_log_lines_2026_11 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-11-01') TO ('2026-12-01');
CREATE TABLE build_log_lines_2026_12 PARTITION OF build_log_lines
    FOR VALUES FROM ('2026-12-01') TO ('2027-01-01');
CREATE TABLE build_log_lines_default PARTITION OF build_log_lines DEFAULT;

-- AUDIT LOG (Partitioned by month)
--
-- Every mutation in the system gets an entry. Partition key in PK required.
-- Old partitions can be detached and archived to S3 after 12–18 months.
CREATE TABLE audit_log (
    id            UUID        NOT NULL,
    workspace_id  UUID        NOT NULL,
    actor_id      UUID,
    action        TEXT        NOT NULL,
    resource_type TEXT        NOT NULL,
    resource_id   UUID,
    metadata      JSONB,
    ip_address    INET,
    user_agent    TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX idx_audit_workspace_created ON audit_log(workspace_id, created_at DESC);
CREATE INDEX idx_audit_actor_created     ON audit_log(actor_id,     created_at DESC);
CREATE INDEX idx_audit_resource          ON audit_log(resource_type, resource_id, created_at DESC);

CREATE TABLE audit_log_2026_03 PARTITION OF audit_log
    FOR VALUES FROM ('2026-03-01') TO ('2026-04-01');
CREATE TABLE audit_log_2026_04 PARTITION OF audit_log
    FOR VALUES FROM ('2026-04-01') TO ('2026-05-01');
CREATE TABLE audit_log_2026_05 PARTITION OF audit_log
    FOR VALUES FROM ('2026-05-01') TO ('2026-06-01');
CREATE TABLE audit_log_2026_06 PARTITION OF audit_log
    FOR VALUES FROM ('2026-06-01') TO ('2026-07-01');
CREATE TABLE audit_log_2026_07 PARTITION OF audit_log
    FOR VALUES FROM ('2026-07-01') TO ('2026-08-01');
CREATE TABLE audit_log_2026_08 PARTITION OF audit_log
    FOR VALUES FROM ('2026-08-01') TO ('2026-09-01');
CREATE TABLE audit_log_2026_09 PARTITION OF audit_log
    FOR VALUES FROM ('2026-09-01') TO ('2026-10-01');
CREATE TABLE audit_log_2026_10 PARTITION OF audit_log
    FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');
CREATE TABLE audit_log_2026_11 PARTITION OF audit_log
    FOR VALUES FROM ('2026-11-01') TO ('2026-12-01');
CREATE TABLE audit_log_2026_12 PARTITION OF audit_log
    FOR VALUES FROM ('2026-12-01') TO ('2027-01-01');
CREATE TABLE audit_log_default PARTITION OF audit_log DEFAULT;

-- USAGE / BILLING
-- Metrics adapted for Liquid Metal: VM hours, Wasm invocations, bandwidth, storage.
CREATE TABLE usage_records (
    id           UUID        PRIMARY KEY,
    workspace_id UUID        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    metric       TEXT        NOT NULL CHECK (metric IN (
                                 'vm_hours',
                                 'wasm_invocations',
                                 'bandwidth_gb',
                                 'storage_gb',
                                 'build_minutes'
                             )),
    quantity     NUMERIC     NOT NULL,
    period_start DATE        NOT NULL,
    period_end   DATE        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_usage_workspace_period ON usage_records(workspace_id, period_start, period_end);
CREATE INDEX idx_usage_workspace_metric ON usage_records(workspace_id, metric, period_start);

-- DATABASE-LEVEL SAFETY SETTINGS (run as superuser against production DB)
-- ALTER DATABASE machinename SET statement_timeout              = '30s';
-- ALTER DATABASE machinename SET lock_timeout                   = '10s';
-- ALTER DATABASE machinename SET idle_in_transaction_session_timeout = '60s';

-- PARTITION MAINTENANCE
--
-- Run monthly via pg_cron or external cron to add next month's partitions:
--
--   CREATE TABLE build_log_lines_2027_01 PARTITION OF build_log_lines
--       FOR VALUES FROM ('2027-01-01') TO ('2027-02-01');
--   CREATE TABLE audit_log_2027_01 PARTITION OF audit_log
--       FOR VALUES FROM ('2027-01-01') TO ('2027-02-01');
--
-- To archive old partitions:
--   ALTER TABLE build_log_lines DETACH PARTITION build_log_lines_2026_03;
--   -- pg_dump → S3, then DROP TABLE build_log_lines_2026_03;
