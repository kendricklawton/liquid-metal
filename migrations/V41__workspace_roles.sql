-- Expand workspace role set: rename 'member' → 'admin' and add 'admin' to allowed values.
-- Hierarchy: owner > admin > viewer
ALTER TABLE workspace_members
  DROP CONSTRAINT workspace_members_role_check;

ALTER TABLE workspace_members
  ADD CONSTRAINT workspace_members_role_check
    CHECK (role IN ('owner', 'admin', 'viewer'));

UPDATE workspace_members SET role = 'admin' WHERE role = 'member';
