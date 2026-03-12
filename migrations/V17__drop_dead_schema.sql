-- Drop tables and columns that were never wired up in application code.
-- usage_records, usage_pulses: billing tables from V1/V4 — never inserted or queried.
-- last_heartbeat_at: V4 column on services — never written or read.
-- balance_credits: V4 column on workspaces — never used (billing not implemented).

DROP TABLE IF EXISTS usage_pulses;
DROP TABLE IF EXISTS usage_records;

ALTER TABLE services    DROP COLUMN IF EXISTS last_heartbeat_at;
ALTER TABLE workspaces  DROP COLUMN IF EXISTS balance_credits;
