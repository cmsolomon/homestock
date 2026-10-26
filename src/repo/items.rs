use crate::{error::AppError, repo::locations::path_text};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct ItemRow {
    pub id: i64, pub name: String, pub location_id: i64, pub qty: i64, pub unit: String,
    pub min_qty: i64, pub photo: Option<String>, pub in_use: i64, pub tags: Vec<String>,
}
impl ItemRow {
    pub fn available(&self) -> i64 { self.qty - self.in_use }
    pub fn is_low(&self) -> bool { self.qty > 0 && self.min_qty > 0 && self.available() <= self.min_qty }
    pub fn is_used_up(&self) -> bool { self.qty == 0 }
}

pub struct NewItem<'a> {
    pub name: &'a str, pub location_id: i64, pub qty: i64, pub unit: &'a str, pub min_qty: i64,
    pub tags: &'a [String], pub notes: Option<&'a str>, pub photo: Option<&'a str>,
}

/// Items at `loc`, or inside it at any depth when `nested`. Used-up items sort last.
pub fn items_in(c: &Connection, loc: i64, nested: bool) -> Result<Vec<ItemRow>, AppError> {
    let scope = if nested {
        "i.location_id IN (WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL
            SELECT l.id FROM location l JOIN sub ON l.parent_id = sub.id) SELECT id FROM sub)"
    } else { "i.location_id = ?1" };
    let sql = format!(
        "SELECT i.id, i.name, i.location_id, i.qty, i.unit, i.min_qty, i.photo,
                COALESCE((SELECT SUM(qty) FROM checkout WHERE item_id = i.id), 0),
                COALESCE((SELECT group_concat(t.name, ',') FROM item_tag it JOIN tag t ON t.id = it.tag_id
                          WHERE it.item_id = i.id), '')
         FROM item i WHERE {scope}
         ORDER BY i.qty = 0, i.name COLLATE NOCASE");
    let mut st = c.prepare_cached(&sql)?;
    let rows = st.query_map([loc], |r| {
        let tags: String = r.get(8)?;
        Ok(ItemRow {
            id: r.get(0)?, name: r.get(1)?, location_id: r.get(2)?, qty: r.get(3)?, unit: r.get(4)?,
            min_qty: r.get(5)?, photo: r.get(6)?, in_use: r.get(7)?,
            tags: tags.split(',').filter(|s| !s.is_empty()).map(String::from).collect(),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn create(c: &mut Connection, n: NewItem, user_id: i64) -> Result<i64, AppError> {
    let name = n.name.trim();
    if name.is_empty() { return Err(AppError::bad("Item name is required")); }
    let tx = c.transaction()?;
    tx.execute(
        "INSERT INTO item(name, location_id, qty, unit, min_qty, notes, photo) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![name, n.location_id, n.qty.max(0), n.unit.trim(), n.min_qty.max(0), n.notes.filter(|s| !s.trim().is_empty()), n.photo])?;
    let id = tx.last_insert_rowid();
    set_tags(&tx, id, n.tags)?;
    // Remember the box for the next Quick add (the “locked breadcrumb”).
    tx.execute("UPDATE user SET last_location_id = ?2 WHERE id = ?1", params![user_id, n.location_id])?;
    reindex(&tx, &[id])?;
    tx.commit()?;
    Ok(id)
}

pub fn set_tags(c: &Connection, item_id: i64, tags: &[String]) -> Result<(), AppError> {
    c.execute("DELETE FROM item_tag WHERE item_id = ?1", [item_id])?;
    for t in tags.iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).filter(|t| !t.is_empty()) {
        c.execute("INSERT INTO tag(name) VALUES (?1) ON CONFLICT(name) DO NOTHING", [&t])?;
        c.execute("INSERT OR IGNORE INTO item_tag(item_id, tag_id) SELECT ?1, id FROM tag WHERE name = ?2", params![item_id, t])?;
    }
    Ok(())
}

/// A same-name (case-insensitive), same-unit item at `to`, other than `src` itself.
fn same_item_at(c: &Connection, src: i64, to: i64) -> Result<Option<i64>, AppError> {
    Ok(c.query_row(
        "SELECT d.id FROM item d JOIN item s ON s.id = ?1
         WHERE d.location_id = ?2 AND d.id <> s.id AND d.name = s.name COLLATE NOCASE AND d.unit = s.unit",
        params![src, to], |r| r.get(0)).optional()?)
}

/// Move the whole record (no checkouts left on it) to `to`. If `to` already holds the same
/// item, fold this record into it: its qty and tags carry over, shopping entries follow
/// (an open entry yields to the destination's own), and this record is removed.
fn relocate(c: &Connection, id: i64, to: i64) -> Result<i64, AppError> {
    let Some(dest) = same_item_at(c, id, to)? else {
        c.execute("UPDATE item SET location_id = ?2, updated_at = unixepoch() WHERE id = ?1", params![id, to])?;
        return Ok(id);
    };
    c.execute("UPDATE item SET qty = qty + (SELECT qty FROM item WHERE id = ?1), updated_at = unixepoch() WHERE id = ?2", params![id, dest])?;
    c.execute("INSERT OR IGNORE INTO item_tag(item_id, tag_id) SELECT ?2, tag_id FROM item_tag WHERE item_id = ?1", params![id, dest])?;
    c.execute("UPDATE OR IGNORE shop SET item_id = ?2 WHERE item_id = ?1", params![id, dest])?;
    c.execute("DELETE FROM shop WHERE item_id = ?1", [id])?;
    c.execute("DELETE FROM item WHERE id = ?1", [id])?;
    Ok(dest)
}

/// Same-name, same-unit item at `to` gets `n` added; otherwise a copy of `src` is created there.
fn add_or_merge(c: &Connection, src: i64, to: i64, n: i64) -> Result<i64, AppError> {
    if let Some(id) = same_item_at(c, src, to)? {
        c.execute("UPDATE item SET qty = qty + ?2, updated_at = unixepoch() WHERE id = ?1", params![id, n])?;
        return Ok(id);
    }
    c.execute(
        "INSERT INTO item(name, notes, location_id, qty, unit, min_qty, photo)
         SELECT name, notes, ?2, ?3, unit, min_qty, photo FROM item WHERE id = ?1",
        params![src, to, n])?;
    let id = c.last_insert_rowid();
    c.execute("INSERT INTO item_tag(item_id, tag_id) SELECT ?2, tag_id FROM item_tag WHERE item_id = ?1", params![src, id])?;
    Ok(id)
}

/// Move a whole item, or split `qty` off it. Checked-out units never move.
pub fn move_item(c: &mut Connection, id: i64, to: i64, qty: Option<i64>) -> Result<i64, AppError> {
    let tx = c.transaction()?;
    let (cur, from, in_use): (i64, i64, i64) = tx.query_row(
        "SELECT qty, location_id, COALESCE((SELECT SUM(qty) FROM checkout WHERE item_id = ?1), 0) FROM item WHERE id = ?1",
        [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    if from == to { return Err(AppError::conflict("It’s already there")); }
    let n = qty.unwrap_or(cur);
    let moved_id = if n >= cur && in_use == 0 {
        relocate(&tx, id, to)?
    } else {
        if n < 1 || n > cur - in_use { return Err(AppError::bad(format!("Only {} available to move", cur - in_use))); }
        tx.execute("UPDATE item SET qty = qty - ?2, updated_at = unixepoch() WHERE id = ?1", params![id, n])?;
        add_or_merge(&tx, id, to, n)?
    };
    reindex(&tx, &[id, moved_id])?;
    tx.commit()?;
    Ok(moved_id)
}

pub fn checkout(c: &Connection, item_id: i64, qty: i64, user_id: i64) -> Result<i64, AppError> {
    let active: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM user WHERE id=?1 AND deleted_at IS NULL)",
        [user_id],
        |r| r.get(0),
    )?;
    if !active { return Err(AppError::bad("Choose an active household member")); }
    // The trigger `checkout_not_over_available` enforces the limit atomically.
    c.execute("INSERT INTO checkout(item_id, qty, user_id) VALUES (?1, ?2, ?3)", params![item_id, qty, user_id])?;
    Ok(c.last_insert_rowid())
}

/// Return to origin: the claim simply disappears.
pub fn return_checkout(c: &Connection, checkout_id: i64) -> Result<(), AppError> {
    if c.execute("DELETE FROM checkout WHERE id = ?1", [checkout_id])? == 0 { return Err(AppError::NotFound); }
    Ok(())
}

/// Return somewhere else: move the record if this checkout is all of it, otherwise split.
pub fn put_away(c: &mut Connection, checkout_id: i64, to: i64) -> Result<i64, AppError> {
    let tx = c.transaction()?;
    let (item_id, n): (i64, i64) = tx.query_row("SELECT item_id, qty FROM checkout WHERE id = ?1", [checkout_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let (qty, in_use, current_location): (i64, i64, i64) = tx.query_row(
        "SELECT qty, (SELECT COALESCE(SUM(qty),0) FROM checkout WHERE item_id = ?1), location_id
         FROM item WHERE id = ?1", [item_id], |r| Ok((r.get(0)?, r.get(1)?,r.get(2)?)))?;
    tx.execute("DELETE FROM checkout WHERE id = ?1", [checkout_id])?;
    if current_location == to {
        tx.commit()?;
        return Ok(item_id);
    }
    let dest = if qty == n && in_use == n {
        relocate(&tx, item_id, to)?
    } else {
        tx.execute("UPDATE item SET qty = qty - ?2, updated_at = unixepoch() WHERE id = ?1", params![item_id, n])?;
        add_or_merge(&tx, item_id, to, n)?
    };
    reindex(&tx, &[item_id, dest])?;
    tx.commit()?;
    Ok(dest)
}

/// Mark used. From a checkout (`checkout_id`) the claim is removed too.
/// Returns true if a shopping entry was created.
pub fn consume(c: &mut Connection, item_id: i64, qty: i64, checkout_id: Option<i64>, add_to_list: bool) -> Result<bool, AppError> {
    let tx = c.transaction()?;
    let n = match checkout_id {
        Some(cid) => {
            let n: i64 = tx.query_row("SELECT qty FROM checkout WHERE id = ?1 AND item_id = ?2", params![cid, item_id], |r| r.get(0))?;
            tx.execute("DELETE FROM checkout WHERE id = ?1", [cid])?;
            n
        }
        None => qty,
    };
    tx.execute("UPDATE item SET qty = MAX(0, qty - ?2), updated_at = unixepoch() WHERE id = ?1", params![item_id, n])?;
    let mut added = false;
    if add_to_list {
        // Suggested buy qty: back above the alert level, or what was used.
        added = tx.execute(
            "INSERT OR IGNORE INTO shop(name, item_id, qty)
             SELECT s.name, s.id,
                    CASE WHEN s.min_qty > 0 THEN MAX(1, s.min_qty - s.available + 1) ELSE MAX(1, ?2) END
             FROM item_status s WHERE s.id = ?1",
            params![item_id, n])? > 0;
    }
    tx.commit()?;
    Ok(added)
}

/// Keep item_fts in sync. Call after any write that touches name/notes/tags/location.
pub fn reindex(c: &Connection, ids: &[i64]) -> Result<(), AppError> {
    let mut del = c.prepare_cached("DELETE FROM item_fts WHERE rowid = ?1")?;
    let mut ins = c.prepare_cached(
        "INSERT INTO item_fts(rowid, name, notes, tags, path)
         SELECT i.id, i.name, COALESCE(i.notes, ''),
                COALESCE((SELECT group_concat(t.name, ' ') FROM item_tag it JOIN tag t ON t.id = it.tag_id WHERE it.item_id = i.id), ''),
                ?2
         FROM item i WHERE i.id = ?1")?;
    for &id in ids {
        del.execute([id])?;
        let loc: Option<i64> = c.query_row("SELECT location_id FROM item WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        if let Some(loc) = loc { ins.execute(params![id, path_text(c, loc)?])?; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql")).expect("apply schema");
        c.execute_batch(include_str!("../../migrations/0002_soft_delete_users.sql")).expect("apply user soft-delete migration");
        c.execute("INSERT INTO user(name,role,password_hash) VALUES ('Jamie','admin','hash')", []).expect("insert user");
        c.execute("INSERT INTO location(name,kind) VALUES ('Garage','Room')", []).expect("insert source location");
        c.execute("INSERT INTO location(name,kind) VALUES ('Kitchen','Room')", []).expect("insert destination location");
        c.execute("INSERT INTO item(name,location_id,qty,unit) VALUES ('Batteries',1,8,'pcs')", []).expect("insert item");
        c
    }

    #[test]
    fn putting_away_at_origin_only_returns_checkout() {
        let mut c = database();
        checkout(&c, 1, 2, 1).expect("checkout units");
        assert_eq!(put_away(&mut c, 1, 1).expect("put away to origin"), 1);
        let qty: i64 = c.query_row("SELECT qty FROM item WHERE id=1", [], |r| r.get(0)).expect("read quantity");
        let checkouts: i64 = c.query_row("SELECT COUNT(*) FROM checkout", [], |r| r.get(0)).expect("count checkouts");
        let items: i64 = c.query_row("SELECT COUNT(*) FROM item", [], |r| r.get(0)).expect("count items");
        assert_eq!((qty, checkouts, items), (8, 0, 1));
    }

    #[test]
    fn putting_away_partial_checkout_splits_inventory() {
        let mut c = database();
        checkout(&c, 1, 2, 1).expect("checkout units");
        put_away(&mut c, 1, 2).expect("put away to another location");
        let rows: Vec<(i64, i64)> = {
            let mut stmt = c.prepare("SELECT location_id,qty FROM item ORDER BY location_id").expect("prepare rows");
            stmt.query_map([], |r| Ok((r.get(0)?,r.get(1)?))).expect("query rows")
                .collect::<Result<_,_>>().expect("collect rows")
        };
        assert_eq!(rows, vec![(1,6),(2,2)]);
    }

    fn add_kitchen_batteries(c: &Connection, qty: i64) {
        c.execute("INSERT INTO item(name,location_id,qty,unit) VALUES ('batteries',2,?1,'pcs')", [qty]).expect("insert destination item");
    }

    fn items(c: &Connection) -> Vec<(i64, i64, i64)> {
        let mut stmt = c.prepare("SELECT id,location_id,qty FROM item ORDER BY id").expect("prepare rows");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).expect("query rows")
            .collect::<Result<_, _>>().expect("collect rows")
    }

    #[test]
    fn moving_a_whole_item_merges_into_a_same_name_item() {
        let mut c = database();
        add_kitchen_batteries(&c, 3);
        set_tags(&c, 1, &["aa".into()]).expect("tag source");
        c.execute("INSERT INTO shop(name,item_id,qty,bought_at) VALUES ('Batteries',1,4,unixepoch())", []).expect("bought entry");
        c.execute("INSERT INTO shop(name,item_id,qty) VALUES ('Batteries',1,2)", []).expect("open entry on source");
        c.execute("INSERT INTO shop(name,item_id,qty) VALUES ('batteries',2,1)", []).expect("open entry on destination");

        assert_eq!(move_item(&mut c, 1, 2, None).expect("move whole item"), 2);

        assert_eq!(items(&c), vec![(2, 2, 11)]);
        let tag: String = c.query_row("SELECT t.name FROM item_tag it JOIN tag t ON t.id=it.tag_id WHERE it.item_id=2", [], |r| r.get(0)).expect("merged tag");
        assert_eq!(tag, "aa");
        let shop: Vec<(i64, bool)> = {
            let mut stmt = c.prepare("SELECT item_id,bought_at IS NOT NULL FROM shop ORDER BY id").expect("prepare shop");
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).expect("query shop").collect::<Result<_, _>>().expect("collect shop")
        };
        assert_eq!(shop, vec![(2, true), (2, false)]);
        let indexed: i64 = c.query_row("SELECT COUNT(*) FROM item_fts WHERE rowid=1", [], |r| r.get(0)).expect("count fts");
        assert_eq!(indexed, 0);
    }

    #[test]
    fn putting_away_a_whole_checkout_merges_into_a_same_name_item() {
        let mut c = database();
        add_kitchen_batteries(&c, 3);
        checkout(&c, 1, 8, 1).expect("check out everything");
        assert_eq!(put_away(&mut c, 1, 2).expect("put away elsewhere"), 2);
        assert_eq!(items(&c), vec![(2, 2, 11)]);
    }

    #[test]
    fn moving_a_whole_item_without_a_match_relocates_it() {
        let mut c = database();
        assert_eq!(move_item(&mut c, 1, 2, None).expect("move whole item"), 1);
        assert_eq!(items(&c), vec![(1, 2, 8)]);
    }

    #[test]
    fn consuming_units_decreases_inventory_quantity() {
        let mut c = database();
        consume(&mut c, 1, 3, None, false).expect("consume units");
        let (qty, available): (i64, i64) = c.query_row(
            "SELECT qty,available FROM item_status WHERE id=1",
            [],
            |r| Ok((r.get(0)?,r.get(1)?)),
        ).expect("read updated quantity");
        assert_eq!((qty, available), (5, 5));
    }
}
