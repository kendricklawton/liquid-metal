-- Persist provision failure details on the service row.
-- failure_reason: human-readable error message from the daemon (e.g. startup probe
--   timeout with serial log tail, SHA mismatch, S3 download error).
-- provision_attempts: incremented on each provision attempt (initial + retries).
--   Used by the daemon to track retry count alongside JetStream max_deliver.
ALTER TABLE services ADD COLUMN IF NOT EXISTS failure_reason TEXT;
ALTER TABLE services ADD COLUMN IF NOT EXISTS provision_attempts INTEGER NOT NULL DEFAULT 0;
