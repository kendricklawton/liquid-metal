-- Aggressive autovacuum for the outbox table.
-- Outbox rows live <1s under normal operation (INSERT, publish, DELETE),
-- creating high dead-tuple churn. Default autovacuum settings can't keep up
-- at 100+ deploys/minute, causing table/index bloat and slower FOR UPDATE SKIP LOCKED.
--
-- scale_factor=0.01: vacuum triggers when 1% of rows are dead (vs default 20%).
-- cost_delay=0: no throttling between vacuum pages — outbox is tiny, finish fast.
ALTER TABLE outbox SET (
    autovacuum_vacuum_scale_factor = 0.01,
    autovacuum_vacuum_cost_delay = 0
);
