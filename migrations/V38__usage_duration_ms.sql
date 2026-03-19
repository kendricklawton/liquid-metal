-- Add duration_ms column for Metal compute-time billing.
-- Stores accumulated wall-clock milliseconds per usage reporting window.
-- Used alongside quantity (invocations) for dual-dimension Metal billing:
--   cost = invocations × $0.60/1M + GB-sec × $0.10/GB-sec
ALTER TABLE usage_events ADD COLUMN duration_ms BIGINT;
