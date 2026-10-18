ALTER TABLE user ADD COLUMN deleted_at INTEGER;
PRAGMA user_version = 2;
