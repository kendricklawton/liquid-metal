-- Run mode: serverless (default, idle timeout) or always-on (pro/team only).
ALTER TABLE services ADD COLUMN IF NOT EXISTS run_mode TEXT NOT NULL DEFAULT 'serverless'
    CHECK (run_mode IN ('serverless', 'always-on'));
