-- Add 'suspended' to the service status constraint.
-- Suspended = balance hit zero, daemon tore down the VM/Wasm.
-- User must add credits to resume.
ALTER TABLE services DROP CONSTRAINT IF EXISTS services_status_check;
ALTER TABLE services ADD CONSTRAINT services_status_check
    CHECK (status IN ('provisioning', 'running', 'stopped', 'failed', 'error', 'canceled', 'suspended'));
