use crate::{auth::{self, AdminUser, CurrentUser}, error::AppError, AppState};
use askama::Template;
use axum::{extract::{Form, Path, State}, response::{Html, Redirect}};
use rand::{rngs::OsRng, Rng};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;

#[derive(Template)]
#[template(path = "household.html")]
pub struct HouseholdPage { pub user: CurrentUser, pub members: Vec<Member> }

pub struct Member { pub id:i64, pub name:String, pub role:String, pub must_change:bool, pub checkouts:i64 }

pub async fn page(State(st): State<AppState>, user: CurrentUser) -> Result<Html<String>, AppError> {
    let members=st.db.call(|c| {
        let mut stmt=c.prepare_cached(
            "SELECT u.id,u.name,u.role,u.must_change_pw,
                    (SELECT COUNT(*) FROM checkout WHERE user_id=u.id)
             FROM user u ORDER BY u.name COLLATE NOCASE",
        )?;
        let rows=stmt.query_map([],|r|Ok(Member{id:r.get(0)?,name:r.get(1)?,role:r.get(2)?,must_change:r.get(3)?,checkouts:r.get(4)?}))?
            .collect::<Result<Vec<_>,_>>()?;
        Ok(rows)
    }).await?;
    Ok(Html(HouseholdPage{user,members}.render()?))
}

fn temp_password() -> String {
    const WORDS:&[&str]=&["maple","river","cedar","meadow","harbor","copper","willow","morning","silver","garden","cabin","cloud"];
    let mut rng=OsRng;
    format!("{}-{}-{:02}",WORDS[rng.gen_range(0..WORDS.len())],WORDS[rng.gen_range(0..WORDS.len())],rng.gen_range(10..100))
}

#[derive(Template)]
#[template(path = "temp_password.html")]
pub struct TempPasswordPage { pub name:String, pub password:String, pub action:String }

#[derive(Deserialize)]
pub struct AddForm { name:String, role:String }

pub async fn create(State(st):State<AppState>, _admin:AdminUser, Form(f):Form<AddForm>)->Result<Html<String>,AppError>{
    let name=f.name.trim().to_owned();
    if name.is_empty() || name.len()>60 { return Err(AppError::bad("Enter a name up to 60 characters long")); }
    if !["admin","member"].contains(&f.role.as_str()) { return Err(AppError::bad("Choose a valid role")); }
    let password=temp_password();
    let password_hash=password.clone();
    let password_hash=tokio::task::spawn_blocking(move||auth::hash(&password_hash)).await.map_err(|e|AppError::internal(e.to_string()))??;
    let saved_name=name.clone();
    st.db.call(move|c|{
        c.execute("INSERT INTO user(name,role,password_hash,must_change_pw) VALUES (?1,?2,?3,1)",params![saved_name,f.role,password_hash])?;
        Ok(())
    }).await?;
    Ok(Html(TempPasswordPage{name,password,action:"Share this temporary password with the new member.".into()}.render()?))
}

pub async fn reset(State(st):State<AppState>,admin:AdminUser,Path(id):Path<i64>)->Result<Html<String>,AppError>{
    if id==admin.0.id { return Err(AppError::bad("You can’t reset your own password here")); }
    let password=temp_password();
    let pw=password.clone();
    let hash=tokio::task::spawn_blocking(move||auth::hash(&pw)).await.map_err(|e|AppError::internal(e.to_string()))??;
    let name=st.db.call(move|c|{
        let tx=c.transaction()?;
        let name=tx.query_row("SELECT name FROM user WHERE id=?1",[id],|r|r.get::<_,String>(0)).optional()?.ok_or(AppError::NotFound)?;
        tx.execute("UPDATE user SET password_hash=?2,must_change_pw=1 WHERE id=?1",params![id,hash])?;
        tx.execute("DELETE FROM session WHERE user_id=?1",[id])?;
        tx.commit()?;
        Ok(name)
    }).await?;
    Ok(Html(TempPasswordPage{name,password,action:"The old password no longer works; open sessions have been signed out.".into()}.render()?))
}

#[derive(Deserialize)]
pub struct RoleForm { role:String }

pub async fn role(State(st):State<AppState>,_admin:AdminUser,Path(id):Path<i64>,Form(f):Form<RoleForm>)->Result<Redirect,AppError>{
    if !["admin","member"].contains(&f.role.as_str()){return Err(AppError::bad("Choose a valid role"));}
    st.db.call(move|c|{
        let tx=c.transaction()?;
        let old:String=tx.query_row("SELECT role FROM user WHERE id=?1",[id],|r|r.get(0))?;
        if old=="admin"&&f.role=="member"{
            let admins:i64=tx.query_row("SELECT COUNT(*) FROM user WHERE role='admin'",[],|r|r.get(0))?;
            if admins<=1{return Err(AppError::conflict("The household must keep at least one admin"));}
        }
        tx.execute("UPDATE user SET role=?2 WHERE id=?1",params![id,f.role])?;
        tx.commit()?;
        Ok(())
    }).await?;
    Ok(Redirect::to("/household"))
}

pub async fn remove(State(st):State<AppState>,admin:AdminUser,Path(id):Path<i64>)->Result<Redirect,AppError>{
    if id==admin.0.id{return Err(AppError::conflict("You can’t remove your own account"));}
    st.db.call(move|c|{
        let tx=c.transaction()?;
        let role:String=tx.query_row("SELECT role FROM user WHERE id=?1",[id],|r|r.get(0))?;
        let checkouts:i64=tx.query_row("SELECT COUNT(*) FROM checkout WHERE user_id=?1",[id],|r|r.get(0))?;
        if checkouts>0{return Err(AppError::conflict("Return this member’s checked-out items before removing them"));}
        if role=="admin"{
            let admins:i64=tx.query_row("SELECT COUNT(*) FROM user WHERE role='admin'",[],|r|r.get(0))?;
            if admins<=1{return Err(AppError::conflict("You can’t remove the last admin"));}
        }
        tx.execute("DELETE FROM session WHERE user_id=?1",[id])?;
        tx.execute("DELETE FROM user WHERE id=?1",[id])?;
        tx.commit()?;
        Ok(())
    }).await?;
    Ok(Redirect::to("/household"))
}
