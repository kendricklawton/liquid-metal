-- Invite codes: single-use tokens that gate new user registration.
-- Existing users re-logging in are never checked against this table.
CREATE TABLE invite_codes (
    code       TEXT PRIMARY KEY,
    used_by    UUID REFERENCES users(id),
    used_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT now()
);
