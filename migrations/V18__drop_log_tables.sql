-- Drop build_log_lines and audit_log.
-- Neither table was ever written to by application code.
-- VictoriaLogs (via Promtail) now captures all daemon/API structured logs.
-- Build output and audit events are available in VictoriaLogs with proper labels.

-- Detach and drop all build_log_lines partitions
DO $$
DECLARE
    part RECORD;
BEGIN
    FOR part IN
        SELECT inhrelid::regclass::text AS name
        FROM pg_inherits
        WHERE inhparent = 'build_log_lines'::regclass
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS %s', part.name);
    END LOOP;
END $$;
DROP TABLE IF EXISTS build_log_lines;

-- Detach and drop all audit_log partitions
DO $$
DECLARE
    part RECORD;
BEGIN
    FOR part IN
        SELECT inhrelid::regclass::text AS name
        FROM pg_inherits
        WHERE inhparent = 'audit_log'::regclass
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS %s', part.name);
    END LOOP;
END $$;
DROP TABLE IF EXISTS audit_log;
