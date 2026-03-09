-- ─── V7: Schema fixes ─────────────────────────────────────────────────────────
--
-- Fix 1: workspaces.tier
--   V1 omitted a tier column on workspaces, but the gRPC handlers reference it
--   in INSERT and SELECT. Add it now with a safe default of 'hobby'.
--
-- Fix 2: projects.repo_url
--   V1 modelled projects after a Git-centric platform (project-platform).
--   Liquid Metal deploys pre-built binaries/Wasm directly — repo_url is not
--   required. Make it nullable so CreateProject works without a Git remote.

ALTER TABLE workspaces
    ADD COLUMN IF NOT EXISTS tier TEXT NOT NULL DEFAULT 'hobby'
        CHECK (tier IN ('hobby', 'pro', 'team'));

ALTER TABLE projects
    ALTER COLUMN repo_url DROP NOT NULL,
    ALTER COLUMN repo_url SET DEFAULT '';
