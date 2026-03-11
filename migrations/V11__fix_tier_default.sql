-- V11: Fix users.tier default value to match the check constraint from V8.
-- V8 changed the CHECK to (hobby, pro, team) and migrated existing rows,
-- but left the column DEFAULT as 'free' which violates the constraint on insert.

ALTER TABLE users ALTER COLUMN tier SET DEFAULT 'hobby';
