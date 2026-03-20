-- Add service_id to domains table (existing V1 table has project_id only).
-- service_id is the foreign key for per-service custom domain binding.
ALTER TABLE domains ADD COLUMN IF NOT EXISTS service_id UUID REFERENCES services(id) ON DELETE CASCADE;

CREATE INDEX IF NOT EXISTS idx_domains_verified ON domains(domain) WHERE is_verified = true;
CREATE INDEX IF NOT EXISTS idx_domains_service  ON domains(service_id);
