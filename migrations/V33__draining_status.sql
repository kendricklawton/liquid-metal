-- Add 'draining' to the service status constraint.
-- Draining = balance depleted, routes evicted, waiting for in-flight requests
-- to complete before full suspend. Transitions to 'suspended' after drain period.
ALTER TABLE services DROP CONSTRAINT IF EXISTS services_status_check;
ALTER TABLE services ADD CONSTRAINT services_status_check
    CHECK (status IN ('provisioning', 'running', 'stopped', 'failed', 'error', 'canceled', 'suspended', 'draining'));
