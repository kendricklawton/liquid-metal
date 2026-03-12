-- Track when a service last received a request.
-- Updated by the daemon when it receives a platform.traffic_pulse event from Pingora.
-- Used by the daemon's idle checker to enforce the 5-minute serverless timeout.
ALTER TABLE services ADD COLUMN IF NOT EXISTS last_request_at TIMESTAMPTZ;
