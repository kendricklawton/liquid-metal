-- Drop node_heartbeats — capacity is now configured via METAL_CAPACITY_MB env
-- var on the API. Node health is monitored by VictoriaMetrics (node_exporter).
DROP TABLE IF EXISTS node_heartbeats;
