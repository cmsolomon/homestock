use crate::{auth::{self, AdminUser, CurrentUser}, error::AppError, AppState};
use askama::Template;
use axum::{extract::{Form, Path, State}, response::{Html, Redirect}};
use rand::{rngs::OsRng, Rng};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;

#[derive(Template)]
#[template(path = "household.html")]
pub struct HouseholdPage { pub user: CurrentUser, pub members: Vec<Member> }

pub struct Member { pub id:i64, pub name:String, pub role:String, pub must_change:bool, pub checkouts:i64, pub can_remove:bool }

pub async fn page(State(st): State<AppState>, user: CurrentUser) -> Result<Html<String>, AppError> {
    let members=st.db.call(|c| {
        let mut stmt=c.prepare_cached(
            "SELECT u.id,u.name,u.role,u.must_change_pw,
                    (SELECT COUNT(*) FROM checkout WHERE user_id=u.id),
                    (SELECT COUNT(*) FROM checkout WHERE user_id=u.id)=0
                    AND (u.role!='admin' OR (SELECT COUNT(*) FROM user WHERE role='admin' AND deleted_at IS NULL)>1)
             FROM user u WHERE u.deleted_at IS NULL ORDER BY u.name COLLATE NOCASE",
        )?;
        let rows=stmt.query_map([],|r|Ok(Member{id:r.get(0)?,name:r.get(1)?,role:r.get(2)?,must_change:r.get(3)?,checkouts:r.get(4)?,can_remove:r.get(5)?}))?
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
        ensure_name_available(c, &saved_name)?;
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
        let name=tx.query_row("SELECT name FROM user WHERE id=?1 AND deleted_at IS NULL",[id],|r|r.get::<_,String>(0)).optional()?.ok_or(AppError::NotFound)?;
        tx.execute("UPDATE user SET password_hash=?2,must_change_pw=1 WHERE id=?1 AND deleted_at IS NULL",params![id,hash])?;
        tx.execute("DELETE FROM session WHERE user_id=?1",[id])?;
        tx.commit()?;
        Ok(name)
    }).await?;
    Ok(Html(TempPasswordPage{name,password,action:"The old password no longer works; open sessions have been signed out.".into()}.render()?))
}

#[derive(Deserialize)]
pub struct PasswordForm { password:String, confirm_password:String }

pub async fn change_password(State(st):State<AppState>,admin:AdminUser,Path(id):Path<i64>,Form(f):Form<PasswordForm>)->Result<Redirect,AppError>{
    if id==admin.0.id{return Err(AppError::bad("Use Account settings to change your own password"));}
    if f.password.len()<12{return Err(AppError::bad("Passwords must be at least 12 characters long"));}
    if f.password.len()>1024{return Err(AppError::bad("Passwords must be 1024 characters or fewer"));}
    if f.password!=f.confirm_password{return Err(AppError::bad("The passwords do not match"));}
    let password=f.password;
    let password_hash=tokio::task::spawn_blocking(move||auth::hash(&password)).await.map_err(|e|AppError::internal(e.to_string()))??;
    st.db.call(move|c|set_password(c,id,&password_hash)).await?;
    Ok(Redirect::to("/household"))
}

fn set_password(c:&mut rusqlite::Connection,id:i64,password_hash:&str)->Result<(),AppError>{
    let tx=c.transaction()?;
    if tx.execute("UPDATE user SET password_hash=?2,must_change_pw=1 WHERE id=?1 AND deleted_at IS NULL",params![id,password_hash])?==0 {
        return Err(AppError::NotFound);
    }
    tx.execute("DELETE FROM session WHERE user_id=?1",[id])?;
    tx.commit()?;
    Ok(())
}

#[derive(Deserialize)]
pub struct RoleForm { role:String }

pub async fn role(State(st):State<AppState>,_admin:AdminUser,Path(id):Path<i64>,Form(f):Form<RoleForm>)->Result<Redirect,AppError>{
    if !["admin","member"].contains(&f.role.as_str()){return Err(AppError::bad("Choose a valid role"));}
    st.db.call(move|c|{
        let tx=c.transaction()?;
        let old:String=tx.query_row("SELECT role FROM user WHERE id=?1 AND deleted_at IS NULL",[id],|r|r.get(0)).optional()?.ok_or(AppError::NotFound)?;
        if old=="admin"&&f.role=="member"{
            let admins:i64=tx.query_row("SELECT COUNT(*) FROM user WHERE role='admin' AND deleted_at IS NULL",[],|r|r.get(0))?;
            if admins<=1{return Err(AppError::conflict("The household must keep at least one admin"));}
        }
        tx.execute("UPDATE user SET role=?2 WHERE id=?1",params![id,f.role])?;
        tx.commit()?;
        Ok(())
    }).await?;
    Ok(Redirect::to("/household"))
}

pub async fn remove(State(st):State<AppState>,admin:AdminUser,Path(id):Path<i64>)->Result<Redirect,AppError>{
    let removing_self=id==admin.0.id;
    st.db.call(move|c|remove_user(c,id)).await?;
    Ok(Redirect::to(if removing_self{"/login"}else{"/household"}))
}

fn remove_user(c:&mut rusqlite::Connection,id:i64)->Result<(),AppError>{
    let tx=c.transaction()?;
    let role:String=tx.query_row("SELECT role FROM user WHERE id=?1 AND deleted_at IS NULL",[id],|r|r.get(0)).optional()?.ok_or(AppError::NotFound)?;
    let checkouts:i64=tx.query_row("SELECT COUNT(*) FROM checkout WHERE user_id=?1",[id],|r|r.get(0))?;
    if checkouts>0{return Err(AppError::conflict("Return this member’s checked-out items before removing them"));}
    if role=="admin"{
        let admins:i64=tx.query_row("SELECT COUNT(*) FROM user WHERE role='admin' AND deleted_at IS NULL",[],|r|r.get(0))?;
        if admins<=1{return Err(AppError::conflict("You can’t remove the last admin"));}
    }
    tx.execute("DELETE FROM session WHERE user_id=?1",[id])?;
    tx.execute("UPDATE user SET deleted_at=unixepoch() WHERE id=?1 AND deleted_at IS NULL",[id])?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn ensure_name_available(c:&rusqlite::Connection,name:&str)->Result<(),AppError>{
    let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM user WHERE name=?1 COLLATE NOCASE)",[name],|r|r.get(0))?;
    if exists{return Err(AppError::conflict("That user name is already taken. Choose a unique name."));}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn database()->Connection{
        let c=Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql")).expect("apply schema");
        c.execute_batch(include_str!("../../migrations/0002_soft_delete_users.sql")).expect("apply user soft-delete migration");
        c.execute("INSERT INTO user(name,role,password_hash) VALUES ('Alex','admin','old')",[]).expect("insert admin");
        c
    }

    #[test]
    fn removing_an_additional_admin_is_allowed_but_last_admin_is_protected(){
        let mut c=database();
        c.execute("INSERT INTO user(name,role,password_hash) VALUES ('Sam','admin','hash')",[]).expect("insert second admin");
        remove_user(&mut c,2).expect("remove extra admin");
        let (name,deleted_at):(String,Option<i64>)=c.query_row("SELECT name,deleted_at FROM user WHERE id=2",[],|r|Ok((r.get(0)?,r.get(1)?))).expect("retained account");
        assert_eq!(name,"Sam");
        assert!(deleted_at.is_some());
        assert!(matches!(ensure_name_available(&c,"sAm"),Err(AppError::Conflict(_))));
        assert!(matches!(remove_user(&mut c,1),Err(AppError::Conflict(_))));
    }

    #[test]
    fn user_names_are_unique_without_case_sensitivity_and_remain_reserved_after_removal(){
        let mut c=database();
        assert!(matches!(ensure_name_available(&c,"aLeX"),Err(AppError::Conflict(_))));
        c.execute("INSERT INTO user(name,role,password_hash) VALUES ('Sam','member','hash')",[]).expect("insert member");
        remove_user(&mut c,2).expect("remove member");
        assert!(matches!(ensure_name_available(&c,"SAM"),Err(AppError::Conflict(_))));
        ensure_name_available(&c,"Taylor").expect("unused name is available");
    }

    #[test]
    fn user_with_checkouts_must_return_them_before_removal(){
        let mut c=database();
        c.execute("INSERT INTO user(name,role,password_hash) VALUES ('Taylor','member','hash')",[]).expect("insert member");
        c.execute("INSERT INTO location(name,kind) VALUES ('Garage','Room')",[]).expect("insert location");
        c.execute("INSERT INTO item(name,location_id,qty) VALUES ('Tools',1,1)",[]).expect("insert item");
        c.execute("INSERT INTO checkout(item_id,qty,user_id) VALUES (1,1,2)",[]).expect("insert checkout");
        assert!(matches!(remove_user(&mut c,2),Err(AppError::Conflict(_))));
    }

    #[test]
    fn changing_password_invalidates_existing_sessions(){
        let mut c=database();
        c.execute("INSERT INTO session(token,user_id,expires_at) VALUES ('session-token',1,9999999999)",[]).expect("insert session");
        set_password(&mut c,1,"new-hash").expect("set password");
        let hash:String=c.query_row("SELECT password_hash FROM user WHERE id=1",[],|r|r.get(0)).expect("read hash");
        let must_change:bool=c.query_row("SELECT must_change_pw FROM user WHERE id=1",[],|r|r.get(0)).expect("read flag");
        let sessions:i64=c.query_row("SELECT COUNT(*) FROM session WHERE user_id=1",[],|r|r.get(0)).expect("count sessions");
        assert_eq!(hash,"new-hash");
        assert!(must_change);
        assert_eq!(sessions,0);
    }
}
