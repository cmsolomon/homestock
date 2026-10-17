use crate::{auth::CurrentUser, error::AppError, repo::{items, locations}, routes::is_htmx, AppState};
use askama::Template;
use axum::{extract::{Path, Query, State}, http::HeaderMap, response::{Html, IntoResponse, Redirect, Response}, Form};
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ScopeQ { pub scope: Option<String> }
impl ScopeQ { pub fn nested(&self) -> bool { self.scope.as_deref() == Some("nested") } }

pub struct Crumb { pub id: i64, pub name: String }
pub struct BoxCard { pub id: i64, pub name: String, pub kind: String, pub photo: Option<String>, pub meta: String }
pub struct ItemCard {
    pub id: i64, pub name: String, pub qty_label: String, pub photo: Option<String>,
    pub in_use: i64, pub is_low: bool, pub is_used_up: bool, pub tags: Vec<String>, pub rel_path: Option<String>,
}

#[derive(Template)]
#[template(path = "location.html")]
pub struct LocationPage {
    pub user: CurrentUser,
    pub loc_id: i64, pub title: String, pub kind: String, pub meta: String,
    pub crumbs: Vec<Crumb>, pub nested: bool,
    pub boxes: Vec<BoxCard>, pub items: Vec<ItemCard>, pub flat_hint: Option<String>,
}

#[derive(Template)]
#[template(path = "_contents.html")]
pub struct Contents { pub loc_id: i64, pub title: String, pub nested: bool, pub boxes: Vec<BoxCard>, pub items: Vec<ItemCard>, pub flat_hint: Option<String> }

#[derive(Template)]
#[template(path = "locations.html")]
pub struct RootPage { pub user: CurrentUser, pub meta: String, pub boxes: Vec<BoxCard> }

#[derive(Template)]
#[template(path = "_new_location.html")]
pub struct NewLocation { pub parent_id: Option<i64>, pub parent_name: String }

#[derive(Template)]
#[template(path = "_edit_location.html")]
pub struct EditLocation { pub id:i64, pub name:String, pub kind:String }

#[derive(Template)]
#[template(path = "_move_location.html")]
pub struct MoveLocation { pub id: i64, pub name: String, pub current: i64, pub scope: String, pub destinations: Vec<MoveDestination> }
pub struct MoveDestination { pub id: String, pub name: String, pub disabled: bool }

fn plural(n: i64, one: &str, many: &str) -> String { format!("{n} {}", if n == 1 { one } else { many }) }

struct Loaded { title: String, kind: String, meta: String, crumbs: Vec<Crumb>, boxes: Vec<BoxCard>, items: Vec<ItemCard>, flat_hint: Option<String> }

fn render_contents(l: Loaded, loc_id: i64, nested: bool) -> Result<String, AppError> {
    let meta = l.meta.clone();
    let contents = Contents {
        loc_id, title: l.title, nested, boxes: l.boxes, items: l.items, flat_hint: l.flat_hint,
    }.render()?;
    Ok(format!(
        "{contents}<div id=\"location-meta\" hx-swap-oob=\"innerHTML\">{}</div>",
        crate::error::html_escape(&meta)
    ))
}

async fn load(st: &AppState, id: i64, nested: bool) -> Result<Loaded, AppError> {
    st.db.call(move |c| {
        let loc = locations::get(c, id)?;
        let path = locations::path(c, id)?;
        let depth = path.len();
        let stats = locations::child_box_stats(c, Some(id))?;
        let rows = items::items_in(c, id, nested)?;
        let total_inside: i64 = if nested { rows.len() as i64 } else { items::items_in(c, id, true)?.len() as i64 };

        // Relative path for nested items, computed from each item's ancestor chain.
        let mut cards = Vec::with_capacity(rows.len());
        for r in rows {
            let rel = if r.location_id != id {
                let p = locations::path(c, r.location_id)?;
                Some(p.into_iter().skip(depth).map(|(_, n)| n).collect::<Vec<_>>().join(" › "))
            } else { None };
            cards.push(ItemCard {
                id: r.id, name: r.name.clone(), qty_label: format!("{} {}", r.qty, r.unit), photo: r.photo.clone(),
                in_use: r.in_use, is_low: r.is_low(), is_used_up: r.is_used_up(),
                tags: r.tags.iter().take(2).cloned().collect(), rel_path: rel,
            });
        }
        let flat_hint = (!nested && cards.is_empty() && !stats.is_empty() && total_inside > 0)
            .then(|| format!("No loose items here. Show all {total_inside} items in nested boxes →"));
        let meta = format!("{}{} in total",
            if stats.is_empty() { String::new() } else { format!("{} · ", plural(stats.len() as i64, "box", "boxes")) },
            plural(total_inside, "item", "items"));
        let boxes = stats.into_iter().map(|b| BoxCard {
            meta: if b.items == 0 && b.boxes == 0 { "Empty".into() } else {
                format!("{}{}", plural(b.items, "item", "items"), if b.boxes > 0 { format!(" · {}", plural(b.boxes, "box", "boxes")) } else { String::new() }) },
            id: b.id, name: b.name, kind: b.kind, photo: b.photo,
        }).collect();
        Ok(Loaded { title: loc.name, kind: loc.kind, meta, crumbs: path.into_iter().map(|(id, name)| Crumb { id, name }).collect(), boxes, items: cards, flat_hint })
    }).await
}

pub async fn contents_oob(st: &AppState, id: i64, nested: bool) -> Result<String, AppError> {
    let l = load(st, id, nested).await?;
    let meta = l.meta.clone();
    let contents = Contents {
        loc_id: id, title: l.title, nested, boxes: l.boxes, items: l.items, flat_hint: l.flat_hint,
    }.render()?;
    Ok(format!(
        "<div id=\"contents\" hx-swap-oob=\"innerHTML\">{contents}</div><div id=\"location-meta\" hx-swap-oob=\"innerHTML\">{}</div>",
        crate::error::html_escape(&meta)
    ))
}

pub async fn show(State(st): State<AppState>, user: CurrentUser, Path(id): Path<i64>, Query(q): Query<ScopeQ>) -> Result<Html<String>, AppError> {
    let nested = q.nested();
    let l = load(&st, id, nested).await?;
    Ok(Html(LocationPage { user, loc_id: id, title: l.title, kind: l.kind, meta: l.meta, crumbs: l.crumbs, nested,
        boxes: l.boxes, items: l.items, flat_hint: l.flat_hint }.render()?))
}

/// Scope toggle target: `hx-get="/loc/{id}/contents?scope=nested" hx-target="#contents" hx-push-url="/loc/{id}?scope=nested"`
pub async fn contents(State(st): State<AppState>, _u: CurrentUser, Path(id): Path<i64>, Query(q): Query<ScopeQ>) -> Result<Html<String>, AppError> {
    let nested = q.nested();
    let l = load(&st, id, nested).await?;
    Ok(Html(render_contents(l, id, nested)?))
}

pub async fn root(State(st): State<AppState>, user: CurrentUser) -> Result<Html<String>, AppError> {
    let boxes = st.db.call(|c| {
        let rows = locations::child_box_stats(c, None)?;
        Ok(rows.into_iter().map(|b| BoxCard {
            meta: if b.items == 0 && b.boxes == 0 { "Empty".into() } else {
                format!("{}{}", plural(b.items, "item", "items"), if b.boxes > 0 { format!(" · {}", plural(b.boxes, "box", "boxes")) } else { String::new() }) },
            id: b.id, name: b.name, kind: b.kind, photo: b.photo,
        }).collect::<Vec<_>>())
    }).await?;
    let count = boxes.len();
    Ok(Html(RootPage { user, meta: plural(count as i64, "room", "rooms"), boxes }.render()?))
}

pub async fn new_root(_user: CurrentUser) -> Result<Html<String>, AppError> {
    Ok(Html(NewLocation { parent_id: None, parent_name: String::new() }.render()?))
}

pub async fn new_child(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>) -> Result<Html<String>, AppError> {
    let parent_name = st.db.call(move |c| Ok(locations::get(c, id)?.name)).await?;
    Ok(Html(NewLocation { parent_id: Some(id), parent_name }.render()?))
}

#[derive(Deserialize)]
pub struct CreateLocationForm { pub name: String, pub kind: Option<String> }

pub async fn create_root(State(st): State<AppState>, _user: CurrentUser, Form(f): Form<CreateLocationForm>) -> Result<Redirect, AppError> {
    let kind = f.kind.unwrap_or_else(|| "Room".into());
    let id = st.db.call(move |c| locations::create(c, &f.name, &kind, None)).await?;
    Ok(Redirect::to(&format!("/loc/{id}")))
}

pub async fn create_child(State(st): State<AppState>, _user: CurrentUser, Path(parent): Path<i64>, Form(f): Form<CreateLocationForm>) -> Result<Redirect, AppError> {
    let kind = f.kind.unwrap_or_else(|| "Box".into());
    let id = st.db.call(move |c| locations::create(c, &f.name, &kind, Some(parent))).await?;
    Ok(Redirect::to(&format!("/loc/{id}")))
}

pub async fn edit(State(st):State<AppState>,_user:CurrentUser,Path(id):Path<i64>)->Result<Html<String>,AppError>{
    let loc=st.db.call(move|c|locations::get(c,id)).await?;
    Ok(Html(EditLocation{id,name:loc.name,kind:loc.kind}.render()?))
}

#[derive(Deserialize)]
pub struct RenameLocationForm { pub name:String }

pub async fn rename(State(st):State<AppState>,_user:CurrentUser,Path(id):Path<i64>,Form(f):Form<RenameLocationForm>)->Result<Redirect,AppError>{
    st.db.call(move|c|locations::rename(c,id,&f.name)).await?;
    Ok(Redirect::to(&format!("/loc/{id}")))
}

pub async fn delete(State(st):State<AppState>,_user:CurrentUser,Path(id):Path<i64>)->Result<Redirect,AppError>{
    let parent=st.db.call(move|c|locations::delete_if_empty(c,id)).await?;
    Ok(Redirect::to(&parent.map(|parent|format!("/loc/{parent}")).unwrap_or_else(||"/loc".into())))
}

pub async fn move_sheet(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>, Query(q): Query<ScopeQ>) -> Result<Html<String>, AppError> {
    let nested = q.nested();
    let (name, parent, subtree, locations) = st.db.call(move |c| {
        let source = locations::get(c,id)?;
        let subtree = locations::subtree_ids(c,id)?;
        let mut stmt = c.prepare_cached("SELECT id,name FROM location ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?
            .collect::<Result<Vec<_>,_>>()?;
        Ok((source.name,source.parent_id,subtree,rows))
    }).await?;
    let destinations = std::iter::once(MoveDestination {
        id:"root".into(), name:"Top level".into(), disabled:parent.is_none(),
    }).chain(locations.into_iter().map(|(loc_id,name)|MoveDestination {
        id:loc_id.to_string(), name, disabled:subtree.contains(&loc_id)||parent==Some(loc_id),
    })).collect();
    Ok(Html(MoveLocation { id, name, current:id, scope:if nested {"nested"}else{"level"}.into(), destinations }.render()?))
}

#[derive(Deserialize)]
pub struct MoveForm { pub parent: String, pub current: Option<i64>, pub scope: Option<String> }

/// Drag & drop and the Move… sheet both post here. Re-renders #contents of the page the
/// user is on (`current`) and pushes a toast out-of-band.
pub async fn move_box(State(st): State<AppState>, _u: CurrentUser, headers: HeaderMap, Path(id): Path<i64>, Form(f): Form<MoveForm>) -> Result<Response, AppError> {
    let parent = if f.parent == "root" { None } else { Some(f.parent.parse::<i64>().map_err(|_| AppError::bad("Bad destination"))?) };
    let (moved, name, dest) = st.db.call(move |c| {
        let n = locations::move_location(c, id, parent)?;
        let name = locations::get(c, id)?.name;
        let dest = match parent { Some(p) => locations::get(c, p)?.name, None => "top level".into() };
        Ok((n, name, dest))
    }).await?;
    let toast = format!(r#"<div id="toast" hx-swap-oob="innerHTML"><div role="status" class="toast">Moved {}{} to {}</div></div>"#,
        crate::error::html_escape(&name), if moved > 0 { format!(" and {}", plural(moved as i64, "item", "items")) } else { String::new() }, crate::error::html_escape(&dest));
    if !is_htmx(&headers) { return Ok(Redirect::to(&format!("/loc/{}", f.current.unwrap_or(id))).into_response()); }
    let current = f.current.unwrap_or(id);
    let nested = f.scope.as_deref() == Some("nested");
    let l = load(&st, current, nested).await?;
    let body = render_contents(l, current, nested)? + &toast;
    Ok(Html(body).into_response())
}
