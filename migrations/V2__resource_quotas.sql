-- ============================================================================
-- V2: Resource quota columns on services
--
-- Triple-Lock enforcement data stored alongside the service record:
--   Layer 1: vcpu + memory_mb (already present — Firecracker enforces at boot)
--   Layer 2: disk_read/write_bps + iops (applied via cgroup v2 io.max)
--   Layer 3: net_ingress/egress_kbps (applied via tc + tbf / eBPF)
--
-- NULL means unlimited for that dimension.
-- ============================================================================

ALTER TABLE services
    ADD COLUMN disk_read_bps    BIGINT,
    ADD COLUMN disk_write_bps   BIGINT,
    ADD COLUMN disk_read_iops   INT,
    ADD COLUMN disk_write_iops  INT,
    ADD COLUMN net_ingress_kbps INT,
    ADD COLUMN net_egress_kbps  INT;

COMMENT ON COLUMN services.disk_read_bps    IS 'cgroup v2 io.max rbps — bytes/sec, NULL = unlimited';
COMMENT ON COLUMN services.disk_write_bps   IS 'cgroup v2 io.max wbps — bytes/sec, NULL = unlimited';
COMMENT ON COLUMN services.disk_read_iops   IS 'cgroup v2 io.max riops — ops/sec, NULL = unlimited';
COMMENT ON COLUMN services.disk_write_iops  IS 'cgroup v2 io.max wiops — ops/sec, NULL = unlimited';
COMMENT ON COLUMN services.net_ingress_kbps IS 'tc tbf ingress rate — kbps, NULL = unlimited';
COMMENT ON COLUMN services.net_egress_kbps  IS 'tc tbf egress rate — kbps, NULL = unlimited';
