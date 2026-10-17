-- Homestock schema v1. Applied at startup when PRAGMA user_version < 1.
-- Connection pragmas (WAL, foreign_keys, busy_timeout…) are set in db.rs, not here.

BEGIN;

CREATE TABLE user (
  id               INTEGER PRIMARY KEY,
  name             TEXT    NOT NULL UNIQUE COLLATE NOCASE,
  role             TEXT    NOT NULL DEFAULT 'member' CHECK (role IN ('admin','member')),
  password_hash    TEXT    NOT NULL,
  must_change_pw   INTEGER NOT NULL DEFAULT 0 CHECK (must_change_pw IN (0,1)),
  theme            TEXT    NOT NULL DEFAULT 'system' CHECK (theme IN ('light','dark','system')),
  last_location_id INTEGER REFERENCES location(id) ON DELETE SET NULL,
  created_at       INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE session (
  token      TEXT    PRIMARY KEY,               -- 32 random bytes, base64url
  user_id    INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  expires_at INTEGER NOT NULL
);
CREATE INDEX session_user ON session(user_id);

-- Adjacency list. Subtrees via recursive CTE (see repo/locations.rs).
CREATE TABLE location (
  id         INTEGER PRIMARY KEY,
  name       TEXT    NOT NULL,
  kind       TEXT    NOT NULL DEFAULT 'Box',    -- Room, Cupboard, Shelf, Box, Bin, Drawers…
  parent_id  INTEGER REFERENCES location(id) ON DELETE RESTRICT,
  photo      TEXT,                              -- 'photos/<sha256>.jpg'
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  CHECK (parent_id IS NULL OR parent_id <> id)
);
CREATE INDEX location_parent ON location(parent_id);

CREATE TABLE item (
  id          INTEGER PRIMARY KEY,
  name        TEXT    NOT NULL,
  notes       TEXT,                             -- free text (Quick add + detail)
  location_id INTEGER NOT NULL REFERENCES location(id) ON DELETE RESTRICT,
  qty         INTEGER NOT NULL DEFAULT 1 CHECK (qty >= 0),
  unit        TEXT    NOT NULL DEFAULT 'pcs',
  min_qty     INTEGER NOT NULL DEFAULT 0 CHECK (min_qty >= 0),   -- low-stock alert level, 0 = off
  photo       TEXT,
  created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
  updated_at  INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX item_location ON item(location_id);
CREATE INDEX item_name     ON item(location_id, name COLLATE NOCASE, unit);

CREATE TABLE tag (
  id   INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE item_tag (
  item_id INTEGER NOT NULL REFERENCES item(id) ON DELETE CASCADE,
  tag_id  INTEGER NOT NULL REFERENCES tag(id)  ON DELETE CASCADE,
  PRIMARY KEY (item_id, tag_id)
) WITHOUT ROWID;
CREATE INDEX item_tag_tag ON item_tag(tag_id);

-- A claim against item.qty. Deleted on return (no history kept).
CREATE TABLE checkout (
  id             INTEGER PRIMARY KEY,
  item_id        INTEGER NOT NULL REFERENCES item(id) ON DELETE CASCADE,
  qty            INTEGER NOT NULL CHECK (qty > 0),
  user_id        INTEGER NOT NULL REFERENCES user(id) ON DELETE RESTRICT,
  checked_out_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX checkout_item ON checkout(item_id);
CREATE INDEX checkout_user ON checkout(user_id);

-- Guard: never check out more than is available.
CREATE TRIGGER checkout_not_over_available
BEFORE INSERT ON checkout
WHEN NEW.qty > (SELECT qty FROM item WHERE id = NEW.item_id)
             - (SELECT COALESCE(SUM(qty),0) FROM checkout WHERE item_id = NEW.item_id)
BEGIN
  SELECT RAISE(ABORT, 'checkout exceeds available quantity');
END;

-- Guard: item.qty may not drop below what is checked out.
CREATE TRIGGER item_qty_not_below_in_use
BEFORE UPDATE OF qty ON item
WHEN NEW.qty < (SELECT COALESCE(SUM(qty),0) FROM checkout WHERE item_id = NEW.id)
BEGIN
  SELECT RAISE(ABORT, 'quantity below checked-out amount');
END;

CREATE TABLE shop (
  id         INTEGER PRIMARY KEY,
  name       TEXT    NOT NULL,
  item_id    INTEGER REFERENCES item(id) ON DELETE SET NULL,   -- NULL = free-text entry
  qty        INTEGER NOT NULL DEFAULT 1 CHECK (qty > 0),
  bought_at  INTEGER,                                         -- NULL = To buy
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
-- One open entry per item (lets consume use INSERT OR IGNORE).
CREATE UNIQUE INDEX shop_one_open_per_item ON shop(item_id)
  WHERE item_id IS NOT NULL AND bought_at IS NULL;

-- Convenience view used by lists, search filters and CSV.
CREATE VIEW item_status AS
SELECT i.*,
       COALESCE(c.in_use, 0)                         AS in_use,
       i.qty - COALESCE(c.in_use, 0)                 AS available,
       (i.qty > 0 AND i.min_qty > 0
          AND i.qty - COALESCE(c.in_use,0) <= i.min_qty) AS is_low,
       (i.qty = 0)                                   AS is_used_up
FROM item i
LEFT JOIN (SELECT item_id, SUM(qty) AS in_use FROM checkout GROUP BY item_id) c
       ON c.item_id = i.id;

-- Search index, maintained by app code (repo::items::reindex) because
-- `path` needs a recursive CTE, which triggers cannot run.
CREATE VIRTUAL TABLE item_fts USING fts5(
  name, notes, tags, path,
  tokenize = 'trigram'
);

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;

PRAGMA user_version = 1;
COMMIT;
