use crate::{auth::CurrentUser, error::AppError, repo::{items, locations}, routes::is_htmx, AppState};
use askama::Template;
use axum::{extract::{Path, Query, State}, http::HeaderMap, response::{Html, IntoResponse, Redirect, Response}, Form};
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ScopeQ { pub scope: Option<String> }
impl ScopeQ { pub fn nested(&self) -> bool { self.scope.as_deref() != Some("level") } }

pub struct Crumb { pub id: i64, pub name: String }
pub struct BoxCard { pub id: i64, pub name: String, pub kind: String, pub photo: Option<String>, pub meta: String }
pub struct ItemCard {
    pub id: i64, pub name: String, pub qty_label: String, pub photo: Option<String>,
    pub in_use: i64, pub is_low: bool, pub is_used_up: bool, pub tags: Vec<String>,
}
pub struct ItemGroup { pub location_id: i64, pub label: String, pub items: Vec<ItemCard> }

#[derive(Template)]
#[template(path = "location.html")]
pub struct LocationPage {
    pub user: CurrentUser,
    pub loc_id: i64, pub title: String, pub kind: String, pub meta: String,
    pub crumbs: Vec<Crumb>, pub nested: bool, pub boxes: Vec<BoxCard>, pub item_groups: Vec<ItemGroup>,
}

#[derive(Template)]
#[template(path = "_contents.html")]
pub struct Contents { pub loc_id: i64, pub title: String, pub nested: bool, pub boxes: Vec<BoxCard>, pub item_groups: Vec<ItemGroup> }

#[derive(Template)]
#[template(path = "_scope_toggle.html")]
pub struct ScopeToggle { pub loc_id: i64, pub nested: bool }

#[derive(Template)]
#[template(path = "locations.html")]
pub struct RootPage { pub user: CurrentUser, pub meta: String, pub nested: bool, pub boxes: Vec<BoxCard>, pub item_groups: Vec<ItemGroup> }

#[derive(Template)]
#[template(path = "_root_contents.html")]
pub struct RootContents { pub boxes: Vec<BoxCard>, pub item_groups: Vec<ItemGroup> }

#[derive(Template)]
#[template(path = "_root_scope_toggle.html")]
pub struct RootScopeToggle { pub nested: bool }

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

struct Loaded { title: String, kind: String, meta: String, crumbs: Vec<Crumb>, boxes: Vec<BoxCard>, item_groups: Vec<ItemGroup> }

fn render_contents(l: Loaded, loc_id: i64, nested: bool) -> Result<String, AppError> {
    let meta = l.meta.clone();
    let contents = Contents {
        loc_id, title: l.title, nested, boxes: l.boxes, item_groups: l.item_groups,
    }.render()?;
    let toggle = ScopeToggle { loc_id, nested }.render()?;
    Ok(format!(
        "{contents}<div id=\"location-meta\" hx-swap-oob=\"innerHTML\">{}</div><div id=\"scope-toggle\" hx-swap-oob=\"innerHTML\">{toggle}</div>",
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

        // Relative paths identify the box that owns each item in the nested view.
        let mut grouped: std::collections::BTreeMap<(bool, String, i64), (String, Vec<ItemCard>)> =
            std::collections::BTreeMap::new();
        for r in rows {
            let label = if r.location_id != id {
                let p = locations::path(c, r.location_id)?;
                p.into_iter().skip(depth).map(|(_, n)| n).collect::<Vec<_>>().join(" › ")
            } else { loc.name.clone() };
            let card = ItemCard {
                id: r.id, name: r.name.clone(), qty_label: format!("{} {}", r.qty, r.unit), photo: r.photo.clone(),
                in_use: r.in_use, is_low: r.is_low(), is_used_up: r.is_used_up(),
                tags: r.tags.iter().take(2).cloned().collect(),
            };
            let key = (r.location_id != id, label.to_lowercase(), r.location_id);
            grouped.entry(key).or_insert_with(|| (label, Vec::new())).1.push(card);
        }
        let item_groups = grouped.into_iter().map(|((_, _, location_id), (label, items))| ItemGroup {
            location_id, label, items,
        }).collect::<Vec<_>>();
        let total_inside: i64 = item_groups.iter().map(|g| g.items.len() as i64).sum();
        let meta = format!("{}{} in total",
            if stats.is_empty() { String::new() } else { format!("{} · ", plural(stats.len() as i64, "box", "boxes")) },
            plural(total_inside, "item", "items"));
        let boxes = stats.into_iter().map(|b| BoxCard {
            meta: if b.items == 0 && b.boxes == 0 { "Empty".into() } else {
                format!("{}{}", plural(b.items, "item", "items"), if b.boxes > 0 { format!(" · {}", plural(b.boxes, "box", "boxes")) } else { String::new() }) },
            id: b.id, name: b.name, kind: b.kind, photo: b.photo,
        }).collect();
        Ok(Loaded { title: loc.name, kind: loc.kind, meta, crumbs: path.into_iter().map(|(id, name)| Crumb { id, name }).collect(), boxes, item_groups })
    }).await
}

pub async fn contents_oob(st: &AppState, id: i64, nested: bool) -> Result<String, AppError> {
    let l = load(st, id, nested).await?;
    let meta = l.meta.clone();
    let contents = Contents {
        loc_id: id, title: l.title, nested, boxes: l.boxes, item_groups: l.item_groups,
    }.render()?;
    let toggle = ScopeToggle { loc_id: id, nested }.render()?;
    Ok(format!(
        "<div id=\"contents\" hx-swap-oob=\"innerHTML\">{contents}</div><div id=\"location-meta\" hx-swap-oob=\"innerHTML\">{}</div><div id=\"scope-toggle\" hx-swap-oob=\"innerHTML\">{toggle}</div>",
        crate::error::html_escape(&meta)
    ))
}

pub async fn show(State(st): State<AppState>, user: CurrentUser, Path(id): Path<i64>, Query(q): Query<ScopeQ>) -> Result<Html<String>, AppError> {
    let nested = q.nested();
    let l = load(&st, id, nested).await?;
    Ok(Html(LocationPage { user, loc_id: id, title: l.title, kind: l.kind, meta: l.meta, crumbs: l.crumbs,
        nested, boxes: l.boxes, item_groups: l.item_groups }.render()?))
}

/// Returns location contents at the selected scope, including updated scope controls.
pub async fn contents(State(st): State<AppState>, _u: CurrentUser, Path(id): Path<i64>, Query(q): Query<ScopeQ>) -> Result<Html<String>, AppError> {
    let nested = q.nested();
    let l = load(&st, id, nested).await?;
    Ok(Html(render_contents(l, id, nested)?))
}

fn root_data(c: &rusqlite::Connection, nested: bool) -> Result<(Vec<BoxCard>, Vec<ItemGroup>), AppError> {
    let rows = locations::child_box_stats(c, None)?;
    let mut groups = std::collections::BTreeMap::<(String, i64), (String, Vec<ItemCard>)>::new();
    for room in &rows {
        for item in items::items_in(c, room.id, nested)? {
            let path = locations::path(c, item.location_id)?;
            let label = path.into_iter().map(|(_, name)| name).collect::<Vec<_>>().join(" › ");
            let is_low = item.is_low();
            let is_used_up = item.is_used_up();
            let card = ItemCard {
                id: item.id, name: item.name, qty_label: format!("{} {}", item.qty, item.unit), photo: item.photo,
                in_use: item.in_use, is_low, is_used_up,
                tags: item.tags.into_iter().take(2).collect(),
            };
            groups.entry((label.to_lowercase(), item.location_id))
                .or_insert_with(|| (label, Vec::new())).1.push(card);
        }
    }
    let item_groups = groups.into_iter().map(|((_, location_id), (label, items))| ItemGroup {
        location_id, label, items,
    }).collect();
    let boxes = rows.into_iter().map(|b| BoxCard {
            meta: if b.items == 0 && b.boxes == 0 { "Empty".into() } else {
                format!("{}{}", plural(b.items, "item", "items"), if b.boxes > 0 { format!(" · {}", plural(b.boxes, "box", "boxes")) } else { String::new() }) },
            id: b.id, name: b.name, kind: b.kind, photo: b.photo,
        }).collect();
    Ok((boxes, item_groups))
}

pub async fn root(State(st): State<AppState>, user: CurrentUser, Query(q): Query<ScopeQ>) -> Result<Html<String>, AppError> {
    let nested = q.nested();
    let (boxes, item_groups) = st.db.call(move |c| root_data(c, nested)).await?;
    let count = boxes.len();
    Ok(Html(RootPage { user, meta: plural(count as i64, "room", "rooms"), nested, boxes, item_groups }.render()?))
}

pub async fn root_contents(State(st): State<AppState>, _user: CurrentUser, Query(q): Query<ScopeQ>) -> Result<Html<String>, AppError> {
    let nested = q.nested();
    let (boxes, item_groups) = st.db.call(move |c| root_data(c, nested)).await?;
    let contents = RootContents { boxes, item_groups }.render()?;
    let toggle = RootScopeToggle { nested }.render()?;
    Ok(Html(format!("{contents}<div id=\"root-scope-toggle\" hx-swap-oob=\"innerHTML\">{toggle}</div>")))
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
    Ok(Html(MoveLocation { id, name, current:id, scope:if q.nested() {"nested"} else {"level"}.into(), destinations }.render()?))
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
    if !is_htmx(&headers) {
        let current = f.current.unwrap_or(id);
        let target = if f.scope.as_deref() == Some("level") {
            format!("/loc/{current}?scope=level")
        } else {
            format!("/loc/{current}")
        };
        return Ok(Redirect::to(&target).into_response());
    }
    let current = f.current.unwrap_or(id);
    let nested = f.scope.as_deref() != Some("level");
    let l = load(&st, current, nested).await?;
    let body = render_contents(l, current, nested)? + &toast;
    Ok(Html(body).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn root_items_include_nested_boxes_only_when_enabled() {
        let c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql")).expect("apply schema");
        c.execute("INSERT INTO location(name,kind) VALUES ('Garage','Room')", []).expect("insert room");
        c.execute("INSERT INTO location(name,kind,parent_id) VALUES ('Shelf','Box',1)", []).expect("insert nested box");
        c.execute("INSERT INTO item(name,location_id,qty) VALUES ('Loose item',1,1)", []).expect("insert room item");
        c.execute("INSERT INTO item(name,location_id,qty) VALUES ('Box item',2,1)", []).expect("insert nested item");

        let (_, direct_groups) = root_data(&c, false).expect("load room items");
        assert_eq!(direct_groups.len(), 1);
        assert_eq!(direct_groups[0].label, "Garage");
        assert_eq!(direct_groups[0].items[0].name, "Loose item");

        let (_, nested_groups) = root_data(&c, true).expect("load nested items");
        assert_eq!(nested_groups.len(), 2);
        assert!(nested_groups.iter().any(|group| group.label == "Garage" && group.items[0].name == "Loose item"));
        assert!(nested_groups.iter().any(|group| group.label == "Garage › Shelf" && group.items[0].name == "Box item"));
    }
}
