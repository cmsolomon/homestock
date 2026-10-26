use crate::{auth::CurrentUser, error::AppError, repo::locations, AppState};
use axum::{extract::{Query, State}, http::{header, HeaderValue}, response::Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ExportQuery {
    location_id: Option<i64>,
}

fn build_csv(c: &rusqlite::Connection, location_id: Option<i64>) -> Result<Vec<u8>, AppError> {
    if let Some(id) = location_id {
        locations::get(c, id)?;
    }
    let mut stmt = c.prepare_cached(
        "WITH RECURSIVE sub(id) AS (
             SELECT id FROM location WHERE id=?1
             UNION ALL SELECT l.id FROM location l JOIN sub ON l.parent_id=sub.id
         )
         SELECT s.id,s.name,s.qty,s.unit,s.available,s.in_use,
                COALESCE((SELECT group_concat(u.name || ' (' || co.qty || ')','; ')
                          FROM checkout co JOIN user u ON u.id=co.user_id WHERE co.item_id=s.id),''),
                s.location_id,
                COALESCE((SELECT group_concat(t.name,'; ') FROM item_tag it JOIN tag t ON t.id=it.tag_id WHERE it.item_id=s.id),'')
         FROM item_status s
         WHERE ?1 IS NULL OR s.location_id IN (SELECT id FROM sub)
         ORDER BY s.id",
    )?;
    let rows = stmt.query_map([location_id], |r| Ok((
        r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,
        r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,String>(6)?,r.get::<_,i64>(7)?,r.get::<_,String>(8)?,
    )))?.collect::<Result<Vec<_>,_>>()?;
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(["id","name","quantity","unit","available","in_use","checked_out_by","location","tags"])
        .map_err(|e|AppError::internal(e.to_string()))?;
    for row in rows {
        writer.write_record([
            row.0.to_string(),cell(row.1),row.2.to_string(),cell(row.3),row.4.to_string(),row.5.to_string(),
            cell(row.6),cell(locations::path_text(c,row.7)?),cell(row.8),
        ]).map_err(|e|AppError::internal(e.to_string()))?;
    }
    writer.into_inner().map_err(|e|AppError::internal(e.to_string()))
}

/// Text cells starting with a formula character get a leading `'` so spreadsheet apps
/// show them as text instead of evaluating them (CSV formula injection).
fn cell(text: String) -> String {
    if text.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{text}") } else { text }
}

pub async fn csv(
    State(st): State<AppState>,
    _user: CurrentUser,
    Query(query): Query<ExportQuery>,
) -> Result<Response, AppError> {
    let location_id = query.location_id;
    let bytes = st.db.call(move |c| build_csv(c, location_id)).await?;
    let filename = location_id.map_or_else(
        || "homestock-inventory.csv".to_owned(),
        |id| format!("homestock-location-{id}-inventory.csv"),
    );
    let mut response = Response::new(axum::body::Body::from(bytes));
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8"));
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .map_err(|e|AppError::internal(e.to_string()))?;
    response.headers_mut().insert(header::CONTENT_DISPOSITION, disposition);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn database() -> Connection {
        let c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql")).expect("apply schema");
        c.execute_batch(include_str!("../../migrations/0002_soft_delete_users.sql")).expect("apply user migration");
        c.execute("INSERT INTO user(name,role,password_hash) VALUES ('Alex','admin','hash')", [])
            .expect("insert user");
        c.execute_batch(
            "INSERT INTO location(name,kind) VALUES ('Garage','Room');
             INSERT INTO location(name,kind,parent_id) VALUES ('Tool chest','Box',1);
             INSERT INTO location(name,kind) VALUES ('Kitchen','Room');
             INSERT INTO item(name,location_id,qty) VALUES ('Hammer',1,1);
             INSERT INTO item(name,location_id,qty) VALUES ('Wrench',2,2);
             INSERT INTO item(name,location_id,qty) VALUES ('Mug',3,4);",
        ).expect("insert inventory");
        c
    }

    #[test]
    fn location_export_includes_nested_items_and_full_box_paths() {
        let c = database();
        let bytes = build_csv(&c, Some(1)).expect("build location export");
        let mut reader = csv::Reader::from_reader(bytes.as_slice());
        let headers = reader.headers().expect("read CSV header");
        assert_eq!(headers.iter().collect::<Vec<_>>(), vec![
            "id","name","quantity","unit","available","in_use","checked_out_by","location","tags",
        ]);
        let rows = reader.records().collect::<Result<Vec<_>,_>>().expect("read CSV rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(&rows[0][1], "Hammer");
        assert_eq!(&rows[0][7], "Garage");
        assert_eq!(&rows[1][1], "Wrench");
        assert_eq!(&rows[1][7], "Garage › Tool chest");
    }

    #[test]
    fn formula_like_text_is_exported_as_text() {
        let c = database();
        c.execute("UPDATE item SET name='=HYPERLINK(\"http://x\")' WHERE id=1", []).expect("rename item");
        let bytes = build_csv(&c, Some(1)).expect("build export");
        let mut reader = csv::Reader::from_reader(bytes.as_slice());
        let rows = reader.records().collect::<Result<Vec<_>,_>>().expect("read CSV rows");
        assert_eq!(&rows[0][1], "'=HYPERLINK(\"http://x\")");
        assert_eq!(&rows[1][1], "Wrench");
    }

    #[test]
    fn all_inventory_scope_includes_every_location() {
        let c = database();
        let bytes = build_csv(&c, None).expect("build all-inventory export");
        let mut reader = csv::Reader::from_reader(bytes.as_slice());
        let rows = reader.records().collect::<Result<Vec<_>,_>>().expect("read CSV rows");
        assert_eq!(rows.len(), 3);
    }
}
