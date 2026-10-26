use crate::{auth::CurrentUser, error::AppError, repo::locations, AppState};
use askama::Template;
use axum::{extract::{Form, Path, Query, State}, http::{header, HeaderMap, HeaderValue}, response::{Html, IntoResponse, Redirect, Response}};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct TabQuery { pub tab: Option<String> }

pub struct ShopRow {
    pub id: i64, pub name: String, pub qty: i64, pub bought: bool,
    pub item_id: Option<i64>, pub location: Option<String>, pub unit: String,
}

pub struct StockRow { pub id: i64, pub name: String, pub available: i64, pub unit: String, pub location: String, pub listed: bool }
pub struct LocationOption { pub id: i64, pub name: String }
pub struct ShareItem { pub name:String, pub qty:i64, pub unit:String, pub location:String }

#[derive(Template)]
#[template(path = "_share_sheet.html")]
pub struct ShareSheet { pub date:String, pub items:Vec<ShareItem> }

#[derive(Template)]
#[template(path = "_stock_row.html")]
pub struct StockRowTemplate { pub row: StockRow, pub tab: String }

#[derive(Template)]
#[template(path = "_shopping_counts.html")]
pub struct ShoppingCounts { pub buy_count: usize }

#[derive(Template)]
#[template(path = "shopping.html")]
pub struct ShoppingPage {
    pub user: CurrentUser, pub tab: String, pub rows: Vec<ShopRow>, pub stock: Vec<StockRow>,
    pub locations: Vec<LocationOption>, pub low_count: usize, pub used_count: usize,
    pub buy_count: usize, pub bought_count: usize,
}

pub async fn index(State(st): State<AppState>, user: CurrentUser, Query(q): Query<TabQuery>) -> Result<Html<String>, AppError> {
    let tab = q.tab.unwrap_or_else(|| "buy".into());
    let (rows, low, used, locations) = st.db.call(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT s.id,s.name,s.qty,s.bought_at IS NOT NULL,s.item_id,
                    i.unit,i.location_id
             FROM shop s LEFT JOIN item i ON i.id=s.item_id
             ORDER BY s.bought_at IS NOT NULL, s.created_at DESC, s.name COLLATE NOCASE",
        )?;
        let raw = stmt.query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,
            r.get::<_,bool>(3)?,r.get::<_,Option<i64>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<i64>>(6)?)))?
            .collect::<Result<Vec<_>,_>>()?;
        let mut rows = Vec::new();
        for r in raw {
            let location = match r.6 { Some(id) => Some(locations::path_text(c,id)?), None => None };
            rows.push(ShopRow { id:r.0,name:r.1,qty:r.2,bought:r.3,item_id:r.4,unit:r.5.unwrap_or_else(||"pcs".into()),location });
        }
        let load_stock = |condition: &str| -> Result<Vec<StockRow>, AppError> {
            let sql = format!(
                "SELECT s.id,s.name,s.available,s.unit,s.location_id,
                        EXISTS(SELECT 1 FROM shop WHERE item_id=s.id AND bought_at IS NULL)
                 FROM item_status s WHERE {condition} ORDER BY s.name COLLATE NOCASE");
            let mut stmt = c.prepare(&sql)?;
            let raw = stmt.query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,
                r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,bool>(5)?)))?
                .collect::<Result<Vec<_>,_>>()?;
            raw.into_iter().map(|r| Ok(StockRow { id:r.0,name:r.1,available:r.2,unit:r.3,location:locations::path_text(c,r.4)?,listed:r.5 })).collect()
        };
        let mut stmt = c.prepare_cached("SELECT id,name FROM location ORDER BY name COLLATE NOCASE")?;
        let locations = stmt.query_map([], |r| Ok(LocationOption { id:r.get(0)?, name:r.get(1)? }))?
            .collect::<Result<Vec<_>,_>>()?;
        Ok((rows, load_stock("s.is_low")?, load_stock("s.is_used_up")?, locations))
    }).await?;
    let buy_count = rows.iter().filter(|r| !r.bought).count();
    let bought_count = rows.iter().filter(|r| r.bought).count();
    let low_count = low.len();
    let used_count = used.len();
    let stock = match tab.as_str() { "low" => low, "used" => used, _ => Vec::new() };
    Ok(Html(ShoppingPage { user, tab, rows, stock, locations, low_count, used_count, buy_count, bought_count }.render()?))
}

#[derive(Deserialize)]
pub struct AddForm { name: String, qty: Option<i64> }

pub async fn add(State(st): State<AppState>, _user: CurrentUser, Form(f): Form<AddForm>) -> Result<Redirect, AppError> {
    let name = f.name.trim().to_string();
    if name.is_empty() { return Err(AppError::bad("Enter something to buy")); }
    let qty = f.qty.unwrap_or(1).max(1);
    st.db.call(move |c| {
        c.execute("INSERT INTO shop(name,qty) VALUES (?1,?2)", params![name,qty])?;
        Ok(())
    }).await?;
    Ok(Redirect::to("/shopping"))
}

pub async fn toggle(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>) -> Result<Redirect, AppError> {
    st.db.call(move |c| toggle_purchase(c, id)).await?;
    Ok(Redirect::to("/shopping"))
}

pub async fn remove(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>) -> Result<Redirect, AppError> {
    st.db.call(move |c| {
        c.execute("DELETE FROM shop WHERE id=?1", [id])?;
        Ok(())
    }).await?;
    Ok(Redirect::to("/shopping"))
}

pub async fn remove_from_item(State(st): State<AppState>, _user: CurrentUser, headers: HeaderMap, Path(id): Path<i64>, Query(q): Query<TabQuery>) -> Result<Response, AppError> {
    let tab = match q.tab.as_deref() { Some("used") => "used", _ => "low" };
    let (row, buy_count) = st.db.call(move |c| remove_open_item(c, id)).await?;
    if !crate::routes::is_htmx(&headers) {
        return Ok(Redirect::to(&format!("/shopping?tab={tab}")).into_response());
    }
    let row_html = StockRowTemplate { row, tab: tab.into() }.render()?;
    let counts = ShoppingCounts { buy_count }.render()?;
    Ok(Html(format!("{row_html}{counts}")).into_response())
}

#[derive(Deserialize)]
pub struct RestockForm { location_id: Option<i64> }

pub async fn restock(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>, Form(f): Form<RestockForm>) -> Result<Redirect, AppError> {
    st.db.call(move |c| {
        let tx = c.transaction()?;
        let (name,item_id,qty): (String,Option<i64>,i64) = tx.query_row(
            "SELECT name,item_id,qty FROM shop WHERE id=?1 AND bought_at IS NOT NULL", [id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?.ok_or_else(||AppError::bad("Mark the item bought before putting it away"))?;
        if let Some(item_id) = item_id {
            tx.execute("UPDATE item SET qty=qty+?2,updated_at=unixepoch() WHERE id=?1",params![item_id,qty])?;
            crate::repo::items::reindex(&tx,&[item_id])?;
        } else {
            let location_id=f.location_id.ok_or_else(||AppError::bad("Choose a location for this item"))?;
            let existing: Option<i64> = tx.query_row(
                "SELECT id FROM item WHERE location_id=?1 AND name=?2 COLLATE NOCASE AND unit='pcs' LIMIT 1",
                params![location_id,name],|r|r.get(0)).optional()?;
            let item_id=if let Some(item_id)=existing {
                tx.execute("UPDATE item SET qty=qty+?2,updated_at=unixepoch() WHERE id=?1",params![item_id,qty])?;
                item_id
            } else {
                tx.execute("INSERT INTO item(name,location_id,qty,unit) VALUES (?1,?2,?3,'pcs')",params![name,location_id,qty])?;
                tx.last_insert_rowid()
            };
            crate::repo::items::reindex(&tx,&[item_id])?;
        }
        tx.execute("DELETE FROM shop WHERE id=?1",[id])?;
        tx.commit()?;
        Ok(())
    }).await?;
    Ok(Redirect::to("/shopping?tab=bought"))
}

pub async fn from_item(State(st): State<AppState>, _user: CurrentUser, headers: HeaderMap, Path(id): Path<i64>, Query(q): Query<TabQuery>) -> Result<Response, AppError> {
    let tab = match q.tab.as_deref() { Some("used") => "used", _ => "low" };
    let (row, buy_count) = st.db.call(move |c| add_item_to_list(c, id)).await?;
    if !crate::routes::is_htmx(&headers) {
        return Ok(Redirect::to(&format!("/shopping?tab={tab}")).into_response());
    }
    let row_html = StockRowTemplate { row, tab: tab.into() }.render()?;
    let counts = ShoppingCounts { buy_count }.render()?;
    Ok(Html(format!("{row_html}{counts}")).into_response())
}

fn add_item_to_list(c: &rusqlite::Connection, id: i64) -> Result<(StockRow, usize), AppError> {
    c.execute(
        "INSERT OR IGNORE INTO shop(name,item_id,qty)
         SELECT name,id,CASE WHEN min_qty>0 THEN MAX(1,min_qty-available+1) ELSE 1 END
         FROM item_status WHERE id=?1",
        [id],
    )?;
    let row = load_stock_item(c, id)?;
    let count: i64 = c.query_row("SELECT COUNT(*) FROM shop WHERE bought_at IS NULL", [], |r| r.get(0))?;
    let buy_count = usize::try_from(count).map_err(|_| AppError::internal("shopping-list count is outside the supported range"))?;
    Ok((row, buy_count))
}

fn remove_open_item(c: &rusqlite::Connection, id: i64) -> Result<(StockRow, usize), AppError> {
    if c.execute("DELETE FROM shop WHERE item_id=?1 AND bought_at IS NULL", [id])? == 0 {
        return Err(AppError::conflict("This item is no longer on the shopping list"));
    }
    let row = load_stock_item(c, id)?;
    let count: i64 = c.query_row("SELECT COUNT(*) FROM shop WHERE bought_at IS NULL", [], |r| r.get(0))?;
    let buy_count = usize::try_from(count).map_err(|_| AppError::internal("shopping-list count is outside the supported range"))?;
    Ok((row, buy_count))
}

/// Tick (To buy → Bought) or un-tick. Un-ticking an item that is already back on the list
/// (e.g. used up again meanwhile) folds into that open entry, keeping the larger quantity,
/// because each item has at most one open entry.
fn toggle_purchase(c: &mut rusqlite::Connection, id: i64) -> Result<(), AppError> {
    let tx = c.transaction()?;
    let (item_id, qty, bought): (Option<i64>, i64, bool) = tx.query_row(
        "SELECT item_id,qty,bought_at IS NOT NULL FROM shop WHERE id=?1", [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?.ok_or(AppError::NotFound)?;
    let open: Option<i64> = match item_id {
        Some(item_id) if bought => tx.query_row(
            "SELECT id FROM shop WHERE item_id=?1 AND bought_at IS NULL", [item_id], |r| r.get(0)).optional()?,
        _ => None,
    };
    if let Some(open) = open {
        tx.execute("UPDATE shop SET qty=MAX(qty,?2) WHERE id=?1", params![open, qty])?;
        tx.execute("DELETE FROM shop WHERE id=?1", [id])?;
    } else {
        tx.execute("UPDATE shop SET bought_at=CASE WHEN bought_at IS NULL THEN unixepoch() ELSE NULL END WHERE id=?1", [id])?;
    }
    tx.commit()?;
    Ok(())
}

fn load_stock_item(c: &rusqlite::Connection, id: i64) -> Result<StockRow, AppError> {
    let raw = c.query_row(
        "SELECT s.id,s.name,s.available,s.unit,s.location_id,
                EXISTS(SELECT 1 FROM shop WHERE item_id=s.id AND bought_at IS NULL)
         FROM item_status s WHERE s.id=?1",
        [id],
        |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,
            r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,bool>(5)?)),
    ).optional()?.ok_or(AppError::NotFound)?;
    Ok(StockRow {
        id:raw.0,name:raw.1,available:raw.2,unit:raw.3,
        location:locations::path_text(c,raw.4)?,listed:raw.5,
    })
}

fn date_label(c:&rusqlite::Connection)->Result<String,AppError>{
    const MONTHS:[&str;12]=["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    let raw:String=c.query_row("SELECT strftime('%d %m','now')",[],|r|r.get(0))?;
    let (day,month)=raw.split_once(' ').ok_or_else(||AppError::internal("SQLite returned an invalid date"))?;
    let month:usize=month.parse().map_err(|e:std::num::ParseIntError|AppError::internal(e.to_string()))?;
    let label=MONTHS.get(month.saturating_sub(1)).ok_or_else(||AppError::internal("SQLite returned an invalid month"))?;
    Ok(format!("{day} {label}"))
}

fn share_text(c:&rusqlite::Connection,include_location:bool)->Result<String,AppError>{
    let date=date_label(c)?;
    let mut stmt=c.prepare_cached(
        "SELECT s.name,s.qty,COALESCE(i.unit,'pcs'),i.location_id
         FROM shop s LEFT JOIN item i ON i.id=s.item_id
         WHERE s.bought_at IS NULL ORDER BY s.created_at,s.name COLLATE NOCASE",
    )?;
    let rows=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<i64>>(3)?)))?
        .collect::<Result<Vec<_>,_>>()?;
    let mut text=format!("Shopping list · {date}\n\n");
    for (name,qty,unit,loc) in rows {
        text.push_str(&format!("☐ {name} — {qty} {unit}"));
        if include_location { if let Some(id)=loc {text.push_str(&format!(" (→ {})",locations::path_text(c,id)?));} }
        text.push('\n');
    }
    text.push_str("\nSent from Homestock");
    Ok(text)
}

pub async fn share_text_route(State(st):State<AppState>,_user:CurrentUser)->Result<Response,AppError>{
    let text=st.db.call(|c|share_text(c,false)).await?;
    let mut response=Response::new(axum::body::Body::from(text));
    response.headers_mut().insert(header::CONTENT_TYPE,HeaderValue::from_static("text/plain; charset=utf-8"));
    Ok(response)
}

pub async fn share_sheet(State(st):State<AppState>,_user:CurrentUser)->Result<Html<String>,AppError>{
    let (date,items)=st.db.call(|c|{
        let date=date_label(c)?;
        let mut stmt=c.prepare_cached(
            "SELECT s.name,s.qty,COALESCE(i.unit,'pcs'),i.location_id FROM shop s LEFT JOIN item i ON i.id=s.item_id
             WHERE s.bought_at IS NULL ORDER BY s.created_at,s.name COLLATE NOCASE",
        )?;
        let raw=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<i64>>(3)?)))?
            .collect::<Result<Vec<_>,_>>()?;
        let items=raw.into_iter().map(|r|Ok(ShareItem{name:r.0,qty:r.1,unit:r.2,location:match r.3{Some(id)=>locations::path_text(c,id)?,None=>String::new()}})).collect::<Result<Vec<_>,AppError>>()?;
        Ok((date,items))
    }).await?;
    Ok(Html(ShareSheet{date,items}.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn listed_low_stock_item_can_be_removed_without_changing_its_stock_status() {
        let c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql")).expect("apply schema");
        c.execute("INSERT INTO location(name,kind) VALUES ('Kitchen','Room')", []).expect("insert room");
        c.execute("INSERT INTO item(name,location_id,qty,min_qty) VALUES ('Coffee',1,2,3)", []).expect("insert low-stock item");

        let (listed, count) = add_item_to_list(&c, 1).expect("add to list");
        assert!(listed.listed);
        assert_eq!(count, 1);

        let (unlisted, count) = remove_open_item(&c, 1).expect("remove from list");
        assert!(!unlisted.listed);
        assert_eq!(count, 0);
        assert!(c.query_row("SELECT is_low FROM item_status WHERE id=1", [], |r| r.get::<_, bool>(0)).expect("read low-stock state"));
    }

    #[test]
    fn removing_an_item_that_is_not_listed_is_reported() {
        let c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql")).expect("apply schema");
        c.execute("INSERT INTO location(name,kind) VALUES ('Kitchen','Room')", []).expect("insert room");
        c.execute("INSERT INTO item(name,location_id,qty,min_qty) VALUES ('Coffee',1,2,3)", []).expect("insert low-stock item");

        assert!(matches!(remove_open_item(&c, 1), Err(AppError::Conflict(_))));
    }

    #[test]
    fn bought_item_can_be_moved_back_to_unbought() {
        let c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql")).expect("apply schema");
        c.execute("INSERT INTO shop(name,bought_at) VALUES ('Coffee',unixepoch())", []).expect("insert bought item");
        let id = c.last_insert_rowid();
        let mut c = c;

        toggle_purchase(&mut c, id).expect("move to unbought");
        let bought: bool = c.query_row("SELECT bought_at IS NOT NULL FROM shop WHERE id=?1", [id], |r| r.get(0)).expect("read state");
        assert!(!bought);
    }

    #[test]
    fn unticking_an_item_already_back_on_the_list_folds_into_the_open_entry() {
        let mut c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql")).expect("apply schema");
        c.execute("INSERT INTO location(name,kind) VALUES ('Kitchen','Room')", []).expect("insert room");
        c.execute("INSERT INTO item(name,location_id,qty) VALUES ('Coffee',1,0)", []).expect("insert item");
        c.execute("INSERT INTO shop(name,item_id,qty,bought_at) VALUES ('Coffee',1,3,unixepoch())", []).expect("bought entry");
        let bought = c.last_insert_rowid();
        c.execute("INSERT INTO shop(name,item_id,qty) VALUES ('Coffee',1,2)", []).expect("new open entry");

        toggle_purchase(&mut c, bought).expect("untick bought entry");

        let rows: Vec<(i64, bool)> = c.prepare("SELECT qty,bought_at IS NOT NULL FROM shop").expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).expect("query").collect::<Result<_, _>>().expect("collect");
        assert_eq!(rows, vec![(3, false)]);
    }
}
