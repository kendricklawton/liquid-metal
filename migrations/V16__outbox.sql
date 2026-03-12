-- Transactional outbox for reliable NATS event delivery.
--
-- The deploy handler INSERTs into this table within the same transaction as the
-- services row. A background task in the API polls for pending rows, publishes
-- each to NATS JetStream, and deletes on ack. If NATS is temporarily unreachable,
-- rows accumulate here and drain automatically when NATS recovers.
--
-- This eliminates the race where a DB commit succeeds but the NATS publish fails,
-- leaving a service stuck in 'provisioning' forever.
CREATE TABLE outbox (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    subject    TEXT        NOT NULL,               -- NATS subject (e.g. platform.provision)
    payload    JSONB       NOT NULL,               -- serialized event body
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- The poller reads oldest-first so events are delivered in insertion order.
CREATE INDEX idx_outbox_created ON outbox (created_at ASC);
