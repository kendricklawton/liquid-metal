-- V24: persist VM metadata for post-restart deprovision recovery
-- Without these columns, a daemon restart loses the fc_pid/cpu_core/vm_id
-- needed to tear down Firecracker VMs on deprovision.

ALTER TABLE services
    ADD COLUMN IF NOT EXISTS fc_pid   INTEGER,
    ADD COLUMN IF NOT EXISTS cpu_core INTEGER,
    ADD COLUMN IF NOT EXISTS vm_id    TEXT;
