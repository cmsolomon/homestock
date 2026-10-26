use crate::{
    auth::CurrentUser,
    error::AppError,
    search::{self, Hit, Status},
    AppState,
};
use askama::Template;
use axum::{
    extract::{Query, State},
    response::Html,
};
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub status: Option<String>,
}

#[derive(Template)]
#[template(path = "search.html")]
pub struct SearchPage {
    pub user: CurrentUser,
    pub query: String,
    pub status: String,
    pub hits: Vec<Hit>,
    pub submitted: bool,
}

pub async fn index(
    State(st): State<AppState>,
    user: CurrentUser,
    Query(q): Query<SearchQuery>,
) -> Result<Html<String>, AppError> {
    let query = q.q.unwrap_or_default();
    let status = q.status.unwrap_or_default();
    let filter = match status.as_str() {
        "in-use" => Some(Status::InUse),
        "low" => Some(Status::Low),
        "used" => Some(Status::UsedUp),
        _ => None,
    };
    let query_db = query.clone();
    let hits = st
        .db
        .call(move |c| search::search(c, &query_db, &[], filter, 100))
        .await?;
    Ok(Html(
        SearchPage {
            user,
            submitted: !query.is_empty() || !status.is_empty(),
            query,
            status,
            hits,
        }
        .render()?,
    ))
}
