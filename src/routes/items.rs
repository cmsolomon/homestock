use crate::{auth::CurrentUser, error::AppError, repo::{items, locations}, AppState};
use askama::Template;
use axum::{extract::{Form, Path, State}, http::HeaderMap, response::{Html, IntoResponse, Redirect, Response}};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;

#[derive(Template)]
#[template(path = "_item_detail.html")]
pub struct Detail {
    pub id: i64,
    pub name: String,
    pub qty: i64,
    pub available: i64,
    pub unit: String,
    pub min_qty: i64,
    pub notes: String,
    pub location: String,
    pub location_id: i64,
    pub tags: String,
    pub people: Vec<Person>,
    pub destinations: Vec<Destination>,
}

#[derive(Template)]
#[template(path = "_consume_item.html")]
pub struct ConsumeItem {
    pub id: i64,
    pub name: String,
    pub unit: String,
    pub available: i64,
}

pub struct Person { pub id: i64, pub name: String }
pub struct Destination { pub id: i64, pub name: String }

#[derive(Template)]
#[template(path = "item.html")]
pub struct ItemPage { pub user: CurrentUser, pub detail: Detail }

pub async fn detail(State(st): State<AppState>, user: CurrentUser, headers: HeaderMap, Path(id): Path<i64>) -> Result<Response, AppError> {
    let (detail, location, tags, people, destinations) = st.db.call(move |c| {
        let detail = c.query_row(
            "SELECT i.name, i.qty, i.qty - COALESCE((SELECT SUM(qty) FROM checkout WHERE item_id=i.id),0),
                    i.unit, i.min_qty, COALESCE(i.notes,''), i.location_id,
                    COALESCE((SELECT group_concat(t.name, ', ') FROM item_tag it JOIN tag t ON t.id=it.tag_id WHERE it.item_id=i.id),'')
             FROM item i WHERE i.id=?1",
            [id], |r| Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)?, r.get::<_,i64>(2)?,
                          r.get::<_,String>(3)?, r.get::<_,i64>(4)?, r.get::<_,String>(5)?, r.get::<_,i64>(6)?, r.get::<_,String>(7)?)),
        ).optional()?.ok_or(AppError::NotFound)?;
        let location = locations::path_text(c, detail.6)?;
        let tags = detail.7.clone();
        let mut stmt = c.prepare_cached("SELECT id,name FROM user WHERE deleted_at IS NULL ORDER BY name COLLATE NOCASE")?;
        let people = stmt.query_map([], |r| Ok(Person { id: r.get(0)?, name: r.get(1)? }))?.collect::<Result<Vec<_>, _>>()?;
        let mut stmt = c.prepare_cached("SELECT id,name FROM location ORDER BY name COLLATE NOCASE")?;
        let destinations = stmt.query_map([], |r| Ok(Destination { id:r.get(0)?, name:r.get(1)? }))?.collect::<Result<Vec<_>,_>>()?;
        Ok((detail, location, tags, people, destinations))
    }).await?;
    let detail = Detail { id, name: detail.0, qty: detail.1, available: detail.2, unit: detail.3, min_qty: detail.4,
        notes: detail.5, location, location_id: detail.6, tags, people, destinations };
    if crate::routes::is_htmx(&headers) { return Ok(Html(detail.render()?).into_response()); }
    Ok(Html(ItemPage { user, detail }.render()?).into_response())
}

#[derive(Deserialize)]
pub struct UpdateForm { name: String, qty: i64, unit: String, min_qty: i64, notes: String, tags: String }

pub async fn update(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>, Form(f): Form<UpdateForm>) -> Result<Redirect, AppError> {
    if f.name.trim().is_empty(){return Err(AppError::bad("Item name is required"));}
    if f.unit.trim().is_empty(){return Err(AppError::bad("Unit is required"));}
    let tags = f.tags.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>();
    let location_id = st.db.call(move |c| {
        let tx = c.transaction()?;
        let location_id: i64 = tx.query_row("SELECT location_id FROM item WHERE id=?1",[id],|r|r.get(0))?;
        tx.execute("UPDATE item SET name=?2, qty=?3, unit=?4, min_qty=?5, notes=?6, updated_at=unixepoch() WHERE id=?1",
            params![id, f.name.trim(), f.qty.max(0), f.unit.trim(), f.min_qty.max(0), f.notes.trim()])?;
        items::set_tags(&tx, id, &tags)?;
        items::reindex(&tx, &[id])?;
        tx.commit()?;
        Ok(location_id)
    }).await?;
    Ok(Redirect::to(&format!("/loc/{location_id}")))
}

pub async fn delete(State(st):State<AppState>,_user:CurrentUser,headers:HeaderMap,Path(id):Path<i64>)->Result<Response,AppError>{
    let location_id=st.db.call(move|c|{
        let tx=c.transaction()?;
        let location_id:i64=tx.query_row("SELECT location_id FROM item WHERE id=?1",[id],|r|r.get(0))?;
        let checkouts:i64=tx.query_row("SELECT COUNT(*) FROM checkout WHERE item_id=?1",[id],|r|r.get(0))?;
        if checkouts>0{return Err(AppError::conflict("Return checked-out units before removing this item"));}
        tx.execute("DELETE FROM item_fts WHERE rowid=?1",[id])?;
        if tx.execute("DELETE FROM item WHERE id=?1",[id])?==0{return Err(AppError::NotFound);}
        tx.commit()?;
        Ok(location_id)
    }).await?;
    if crate::routes::is_htmx(&headers) {
        let mut response=Html(String::new()).into_response();
        response.headers_mut().insert("HX-Redirect",format!("/loc/{location_id}").parse().map_err(|e|AppError::internal(format!("invalid redirect header: {e}")))?);
        Ok(response)
    }else{
        Ok(Redirect::to(&format!("/loc/{location_id}")).into_response())
    }
}

#[derive(Deserialize)]
pub struct CheckoutForm { qty: i64, user_id: i64 }

pub async fn checkout(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>, Form(f): Form<CheckoutForm>) -> Result<Redirect, AppError> {
    if f.qty < 1 { return Err(AppError::bad("Choose at least one item")); }
    st.db.call(move |c| items::checkout(c, id, f.qty, f.user_id)).await?;
    Ok(Redirect::to("/in-use"))
}

pub async fn consume_dialog(
    State(st): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let item = st.db.call(move |c| {
        c.query_row(
            "SELECT name,unit,available FROM item_status WHERE id=?1",
            [id],
            |r| Ok(ConsumeItem {
                id,
                name: r.get(0)?,
                unit: r.get(1)?,
                available: r.get(2)?,
            }),
        ).optional()?.ok_or(AppError::NotFound)
    }).await?;
    if item.available < 1 {
        return Err(AppError::conflict("There are no available units to consume"));
    }
    Ok(Html(item.render()?))
}

#[derive(Deserialize)]
pub struct MoveForm { location_id: i64, qty: Option<i64> }

pub async fn move_item(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>, Form(f): Form<MoveForm>) -> Result<Redirect, AppError> {
    st.db.call(move |c| crate::repo::items::move_item(c,id,f.location_id,f.qty)).await?;
    Ok(Redirect::to(&format!("/loc/{}",f.location_id)))
}

#[derive(Deserialize)]
pub struct ConsumeForm { qty: i64, add_to_list: Option<String> }

pub async fn consume(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>, Form(f): Form<ConsumeForm>) -> Result<Redirect, AppError> {
    if f.qty < 1 { return Err(AppError::bad("Choose at least one item")); }
    let add = f.add_to_list.is_some();
    let location_id = st.db.call(move |c| {
        let (location_id, available): (i64,i64) = c.query_row(
            "SELECT location_id,available FROM item_status WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if f.qty > available { return Err(AppError::bad(format!("Only {available} available to use"))); }
        crate::repo::items::consume(c,id,f.qty,None,add)?;
        Ok(location_id)
    }).await?;
    Ok(Redirect::to(&format!("/loc/{location_id}")))
}

#[derive(Template)]
#[template(path = "in_use.html")]
pub struct InUsePage { pub user: CurrentUser, pub entries: Vec<CheckoutRow>, pub people: Vec<Person>, pub destinations: Vec<Destination> }

pub struct CheckoutRow {
    pub id: i64, pub item: String, pub qty: i64, pub unit: String,
    pub person: String, pub location: String,
}

pub async fn in_use(State(st): State<AppState>, user: CurrentUser) -> Result<Html<String>, AppError> {
    let (entries,people,destinations) = st.db.call(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT co.id, i.name, co.qty, i.unit, u.name, i.location_id
             FROM checkout co JOIN item i ON i.id=co.item_id JOIN user u ON u.id=co.user_id
             ORDER BY u.name COLLATE NOCASE, i.name COLLATE NOCASE",
        )?;
        let raw = stmt.query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,
            r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?)))?
            .collect::<Result<Vec<_>,_>>()?;
        let entries=raw.into_iter().map(|r| Ok(CheckoutRow { id:r.0,item:r.1,qty:r.2,unit:r.3,person:r.4,location:locations::path_text(c,r.5)? })).collect::<Result<Vec<_>,AppError>>()?;
        let mut stmt=c.prepare_cached("SELECT id,name FROM user WHERE deleted_at IS NULL ORDER BY name COLLATE NOCASE")?;
        let people=stmt.query_map([],|r|Ok(Person{id:r.get(0)?,name:r.get(1)?}))?.collect::<Result<Vec<_>,_>>()?;
        let mut stmt=c.prepare_cached("SELECT id,name FROM location ORDER BY name COLLATE NOCASE")?;
        let destinations=stmt.query_map([],|r|Ok(Destination{id:r.get(0)?,name:r.get(1)?}))?.collect::<Result<Vec<_>,_>>()?;
        Ok((entries,people,destinations))
    }).await?;
    Ok(Html(InUsePage { user, entries, people, destinations }.render()?))
}

pub async fn return_item(State(st): State<AppState>, _user: CurrentUser, Path(id): Path<i64>) -> Result<Redirect, AppError> {
    st.db.call(move |c| items::return_checkout(c, id)).await?;
    Ok(Redirect::to("/in-use"))
}

#[derive(Deserialize)]
pub struct ReassignForm { user_id:i64 }
pub async fn reassign(State(st):State<AppState>,_user:CurrentUser,Path(id):Path<i64>,Form(f):Form<ReassignForm>)->Result<Redirect,AppError>{
    st.db.call(move|c|{
        let active:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM user WHERE id=?1 AND deleted_at IS NULL)",[f.user_id],|r|r.get(0))?;
        if !active{return Err(AppError::bad("Choose an active household member"));}
        if c.execute("UPDATE checkout SET user_id=?2 WHERE id=?1",params![id,f.user_id])?==0{return Err(AppError::NotFound);}
        Ok(())
    }).await?;
    Ok(Redirect::to("/in-use"))
}

#[derive(Deserialize)]
pub struct PutAwayForm { location_id:i64 }
pub async fn put_away(State(st):State<AppState>,_user:CurrentUser,Path(id):Path<i64>,Form(f):Form<PutAwayForm>)->Result<Redirect,AppError>{
    st.db.call(move|c|items::put_away(c,id,f.location_id)).await?;
    Ok(Redirect::to("/in-use"))
}

#[derive(Deserialize)]
pub struct CheckoutConsumeForm { add_to_list:Option<String> }
pub async fn consume_checkout(State(st):State<AppState>,_user:CurrentUser,Path(id):Path<i64>,Form(f):Form<CheckoutConsumeForm>)->Result<Redirect,AppError>{
    let add=f.add_to_list.is_some();
    st.db.call(move|c|{
        let item_id:i64=c.query_row("SELECT item_id FROM checkout WHERE id=?1",[id],|r|r.get(0))?;
        items::consume(c,item_id,0,Some(id),add)?;
        Ok(())
    }).await?;
    Ok(Redirect::to("/in-use"))
}
