use crate::{auth::CurrentUser, error::AppError, photos, repo::{items, locations}, routes::locations as location_routes, AppState};
use askama::Template;
use axum::{extract::{Multipart, Path, Query, State}, response::{Html, IntoResponse, Redirect, Response}};
use serde::Deserialize;

pub struct LocOpt { pub id: i64, pub name: String, pub depth: usize }

#[derive(Template)]
#[template(path = "quick_add.html")]
pub struct Sheet { pub loc_id: i64, pub loc_name: String, pub path: String, pub sticky_tags: String, pub nested: bool }

#[derive(Deserialize, Default)]
pub struct QuickAddQuery { pub scope: Option<String> }
impl QuickAddQuery { fn nested(&self) -> bool { self.scope.as_deref() == Some("nested") } }

#[derive(Template)]
#[template(path = "_qa_row.html")]
pub struct QaRow { pub id: i64, pub name: String, pub qty_label: String, pub photo: Option<String> }

/// GET /loc/{id}/quick-add → the sheet partial, locked to {id}.
pub async fn sheet(State(st): State<AppState>, _u: CurrentUser, Path(id): Path<i64>, Query(q): Query<QuickAddQuery>) -> Result<Html<String>, AppError> {
    let (name, path) = st.db.call(move |c| Ok((locations::get(c, id)?.name, locations::path_text(c, id)?))).await?;
    Ok(Html(Sheet { loc_id: id, loc_name: name, path, sticky_tags: String::new(), nested: q.nested() }.render()?))
}

/// GET /quick-add → the global “+”: reopen on the last box this user added to.
pub async fn last(State(st): State<AppState>, u: CurrentUser) -> Result<Response, AppError> {
    let uid = u.id;
    let last = st.db.call(move |c| locations::last_for_user(c, uid)).await?;
    Ok(match last {
        Some(id) => Redirect::to(&format!("/loc/{id}?qa=1")).into_response(),
        None => Redirect::to("/loc").into_response(),
    })
}

/// POST /loc/{id}/quick-add (multipart). Returns one `<li>` for the session list
/// (`hx-swap="afterbegin"` on #qa-session) plus an OOB count update.
pub async fn create(State(st): State<AppState>, u: CurrentUser, Path(loc_id): Path<i64>, Query(q): Query<QuickAddQuery>, mut mp: Multipart) -> Result<Html<String>, AppError> {
    let (mut name, mut qty, mut unit, mut min_qty, mut tags, mut notes, mut photo_bytes) =
        (String::new(), 1i64, "pcs".to_string(), 0i64, Vec::<String>::new(), String::new(), None::<Vec<u8>>);
    while let Some(field) = mp.next_field().await.map_err(|e| AppError::bad(e.to_string()))? {
        match field.name().unwrap_or("") {
            "name" => name = field.text().await.unwrap_or_default(),
            "qty" => qty = field.text().await.unwrap_or_default().parse().unwrap_or(1).max(1),
            "unit" => { let v = field.text().await.unwrap_or_default(); if !v.trim().is_empty() { unit = v; } }
            "min_qty" => min_qty = field.text().await.unwrap_or_default().parse().unwrap_or(0).max(0),
            "tags" => tags = field.text().await.unwrap_or_default().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "notes" => notes = field.text().await.unwrap_or_default(),
            "photo" => { let b = field.bytes().await.map_err(|e| AppError::bad(e.to_string()))?; if !b.is_empty() { photo_bytes = Some(b.to_vec()); } }
            _ => {}
        }
    }
    if name.trim().is_empty() { return Err(AppError::bad("Item name is required")); }

    // Resize + content-address off the async thread.
    let photo = match photo_bytes {
        Some(b) => Some(photos::store(st.cfg.data_dir.join("photos"), b).await?),
        None => None,
    };

    let uid = u.id;
    let (id, qty_label, photo_ret, name_ret) = {
        let (n, un, tg, nt, ph) = (name.clone(), unit.clone(), tags.clone(), notes.clone(), photo.clone());
        st.db.call(move |c| {
            let id = items::create(c, items::NewItem {
                name: &n, location_id: loc_id, qty, unit: &un, min_qty, tags: &tg,
                notes: Some(nt.as_str()), photo: ph.as_deref(),
            }, uid)?;
            Ok((id, format!("{qty} {}", un.trim()), ph, n.trim().to_string()))
        }).await?
    };
    let mut response = QaRow { id, name: name_ret, qty_label, photo: photo_ret }.render()?;
    response.push_str(&location_routes::contents_oob(&st, loc_id, q.nested()).await?);
    Ok(Html(response))
}
