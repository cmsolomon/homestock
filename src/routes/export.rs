use crate::{auth::CurrentUser, error::AppError, repo::locations, AppState};
use axum::{extract::State, http::{header, HeaderValue}, response::Response};

pub async fn csv(State(st): State<AppState>, _user: CurrentUser) -> Result<Response, AppError> {
    let bytes = st.db.call(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT s.id,s.name,s.qty,s.unit,s.available,s.in_use,
                    COALESCE((SELECT group_concat(u.name || ' (' || co.qty || ')','; ')
                              FROM checkout co JOIN user u ON u.id=co.user_id WHERE co.item_id=s.id),''),
                    s.location_id,
                    COALESCE((SELECT group_concat(t.name,'; ') FROM item_tag it JOIN tag t ON t.id=it.tag_id WHERE it.item_id=s.id),'')
             FROM item_status s ORDER BY s.id",
        )?;
        let rows = stmt.query_map([], |r| Ok((
            r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,
            r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,String>(6)?,r.get::<_,i64>(7)?,r.get::<_,String>(8)?,
        )))?.collect::<Result<Vec<_>,_>>()?;
        let mut writer = csv::Writer::from_writer(Vec::new());
        writer.write_record(["id","name","quantity","unit","available","in_use","checked_out_by","location","tags"])
            .map_err(|e|AppError::internal(e.to_string()))?;
        for row in rows {
            writer.write_record([
                row.0.to_string(),row.1,row.2.to_string(),row.3,row.4.to_string(),row.5.to_string(),
                row.6,locations::path_text(c,row.7)?,row.8,
            ]).map_err(|e|AppError::internal(e.to_string()))?;
        }
        writer.into_inner().map_err(|e|AppError::internal(e.to_string()))
    }).await?;
    let mut response = Response::new(axum::body::Body::from(bytes));
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8"));
    response.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"homestock-inventory.csv\""));
    Ok(response)
}
