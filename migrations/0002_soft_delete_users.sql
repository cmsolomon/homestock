-- One transaction, so a crash can't leave the column added with user_version still at 1
-- (which would fail with "duplicate column" on every later start).
BEGIN;
ALTER TABLE user ADD COLUMN deleted_at INTEGER;
PRAGMA user_version = 2;
COMMIT;
