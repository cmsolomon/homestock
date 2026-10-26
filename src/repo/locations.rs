use crate::{error::AppError, repo::items::reindex};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct Location {
    pub name: String,
    pub kind: String,
    pub parent_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct BoxStats {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub photo: Option<String>,
    pub items: i64,
    pub boxes: i64,
}

const SUBTREE: &str = "WITH RECURSIVE sub(id) AS (
    SELECT ?1 UNION ALL SELECT l.id FROM location l JOIN sub ON l.parent_id = sub.id)";

pub fn get(c: &Connection, id: i64) -> Result<Location, AppError> {
    Ok(c.query_row(
        "SELECT name,kind,parent_id FROM location WHERE id=?1",
        [id],
        |r| {
            Ok(Location {
                name: r.get(0)?,
                kind: r.get(1)?,
                parent_id: r.get(2)?,
            })
        },
    )?)
}

/// Ancestors root → self, for breadcrumbs.
pub fn path(c: &Connection, id: i64) -> Result<Vec<(i64, String)>, AppError> {
    let mut st = c.prepare_cached(
        "WITH RECURSIVE up(id, name, parent_id, depth) AS (
             SELECT id, name, parent_id, 0 FROM location WHERE id = ?1
             UNION ALL
             SELECT l.id, l.name, l.parent_id, up.depth + 1 FROM location l JOIN up ON l.id = up.parent_id)
         SELECT id, name FROM up ORDER BY depth DESC")?;
    let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn path_text(c: &Connection, id: i64) -> Result<String, AppError> {
    Ok(path(c, id)?
        .into_iter()
        .map(|(_, n)| n)
        .collect::<Vec<_>>()
        .join(" › "))
}

/// All location ids inside `id`, including `id`.
pub fn subtree_ids(c: &Connection, id: i64) -> Result<Vec<i64>, AppError> {
    let mut st = c.prepare_cached(&format!("{SUBTREE} SELECT id FROM sub"))?;
    let rows = st.query_map([id], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Direct children of `parent` (None = rooms) with recursive item and box counts,
/// in one query. Feeds “4 items · 1 box” on box cards.
pub fn child_box_stats(c: &Connection, parent: Option<i64>) -> Result<Vec<BoxStats>, AppError> {
    let mut st = c.prepare_cached(
        "WITH RECURSIVE t(root, id) AS (
             SELECT id, id FROM location WHERE parent_id IS ?1
             UNION ALL
             SELECT t.root, l.id FROM location l JOIN t ON l.parent_id = t.id)
         SELECT r.id, r.name, r.kind, r.photo,
                COUNT(DISTINCT i.id)      AS items,
                COUNT(DISTINCT t.id) - 1  AS boxes
         FROM t
         JOIN location r ON r.id = t.root
         LEFT JOIN item i ON i.location_id = t.id
         GROUP BY r.id
         ORDER BY r.name COLLATE NOCASE",
    )?;
    let rows = st.query_map([parent], |r| {
        Ok(BoxStats {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            photo: r.get(3)?,
            items: r.get(4)?,
            boxes: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn create(
    c: &Connection,
    name: &str,
    kind: &str,
    parent: Option<i64>,
) -> Result<i64, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::bad("Give it a name"));
    }
    if name.len() > 100 {
        return Err(AppError::bad(
            "Location names must be 100 characters or fewer",
        ));
    }
    c.execute(
        "INSERT INTO location(name, kind, parent_id) VALUES (?1, ?2, ?3)",
        params![name, kind, parent],
    )?;
    Ok(c.last_insert_rowid())
}

pub fn rename(c: &mut Connection, id: i64, name: &str) -> Result<(), AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::bad("Give the location a name"));
    }
    if name.len() > 100 {
        return Err(AppError::bad(
            "Location names must be 100 characters or fewer",
        ));
    }
    let tx = c.transaction()?;
    if tx.execute("UPDATE location SET name=?2 WHERE id=?1", params![id, name])? == 0 {
        return Err(AppError::NotFound);
    }
    let item_ids = {
        let mut stmt=tx.prepare_cached(
            "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL SELECT l.id FROM location l JOIN sub ON l.parent_id=sub.id)
             SELECT i.id FROM item i WHERE i.location_id IN (SELECT id FROM sub)",
        )?;
        let rows = stmt
            .query_map([id], |r| r.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        rows
    };
    reindex(&tx, &item_ids)?;
    tx.commit()?;
    Ok(())
}

pub fn delete_if_empty(c: &mut Connection, id: i64) -> Result<Option<i64>, AppError> {
    let tx = c.transaction()?;
    let (parent, children, items): (Option<i64>, i64, i64) = tx.query_row(
        "SELECT (SELECT parent_id FROM location WHERE id=?1),
                (SELECT COUNT(*) FROM location WHERE parent_id=?1),
                (SELECT COUNT(*) FROM item WHERE location_id=?1)",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if children > 0 || items > 0 {
        return Err(AppError::conflict(
            "This location isn’t empty. Move or remove its items and child locations first.",
        ));
    }
    if tx.execute("DELETE FROM location WHERE id=?1", [id])? == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit()?;
    Ok(parent)
}

/// Move a box and everything inside it. O(1) write + reindex of the subtree's items.
/// Returns the number of items that moved with it (for the toast).
pub fn move_location(
    c: &mut Connection,
    id: i64,
    new_parent: Option<i64>,
) -> Result<usize, AppError> {
    let tx = c.transaction()?;
    if let Some(p) = new_parent {
        let cycle: bool = tx.query_row(
            &format!("{SUBTREE} SELECT EXISTS(SELECT 1 FROM sub WHERE id = ?2)"),
            params![id, p],
            |r| r.get(0),
        )?;
        if cycle {
            return Err(AppError::conflict("A box can’t go inside itself"));
        }
    }
    let current: Option<i64> =
        tx.query_row("SELECT parent_id FROM location WHERE id=?1", [id], |r| {
            r.get(0)
        })?;
    if current == new_parent {
        return Err(AppError::conflict("It’s already there"));
    }
    tx.execute(
        "UPDATE location SET parent_id = ?2 WHERE id = ?1",
        params![id, new_parent],
    )?;

    let ids: Vec<i64> = {
        let mut st = tx.prepare(&format!(
            "{SUBTREE} SELECT i.id FROM item i WHERE i.location_id IN (SELECT id FROM sub)"
        ))?;
        let rows = st.query_map([id], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    reindex(&tx, &ids)?;
    tx.commit()?;
    Ok(ids.len())
}

pub fn last_for_user(c: &Connection, user_id: i64) -> Result<Option<i64>, AppError> {
    Ok(c.query_row(
        "SELECT last_location_id FROM user WHERE id=?1",
        [user_id],
        |r| r.get(0),
    )
    .optional()?
    .flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql"))
            .expect("apply schema");
        c.execute(
            "INSERT INTO location(name,kind) VALUES ('Garage','Room')",
            [],
        )
        .expect("insert room");
        c
    }

    #[test]
    fn nonempty_location_cannot_be_deleted() {
        let mut c = database();
        c.execute(
            "INSERT INTO item(name,location_id,qty) VALUES ('Tools',1,1)",
            [],
        )
        .expect("insert item");
        assert!(matches!(
            delete_if_empty(&mut c, 1),
            Err(AppError::Conflict(_))
        ));
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM location WHERE id=1", [], |r| r.get(0))
            .expect("count room");
        assert_eq!(count, 1);
    }

    #[test]
    fn location_with_child_cannot_be_deleted() {
        let mut c = database();
        c.execute(
            "INSERT INTO location(name,kind,parent_id) VALUES ('Shelf','Shelf',1)",
            [],
        )
        .expect("insert child");
        assert!(matches!(
            delete_if_empty(&mut c, 1),
            Err(AppError::Conflict(_))
        ));
    }

    #[test]
    fn empty_location_can_be_deleted_and_renamed() {
        let mut c = database();
        rename(&mut c, 1, "  Store room ").expect("rename room");
        let name: String = c
            .query_row("SELECT name FROM location WHERE id=1", [], |r| r.get(0))
            .expect("read name");
        assert_eq!(name, "Store room");
        delete_if_empty(&mut c, 1).expect("delete empty room");
        assert!(matches!(get(&c, 1), Err(AppError::NotFound)));
    }
}
