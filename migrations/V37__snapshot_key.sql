-- Add snapshot_key column for Metal serverless (Firecracker snapshot restore).
-- After a Metal deploy, the daemon boots the VM, snapshots it, halts it, and
-- stores the S3 key prefix here. The next request restores from snapshot.
ALTER TABLE services ADD COLUMN snapshot_key TEXT;

-- Add 'ready' to the service status enum.
-- 'ready' = deployed + snapshot stored + VM halted + awaiting first request.
ALTER TABLE services DROP CONSTRAINT IF EXISTS services_status_check;
ALTER TABLE services ADD CONSTRAINT services_status_check
    CHECK (status IN ('provisioning', 'running', 'ready', 'stopped', 'failed', 'error', 'canceled', 'suspended', 'draining'));
