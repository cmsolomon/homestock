//! Command-line entry points besides serving: what the upgrade script needs (a verified
//! backup and schema versions) and account recovery for someone with access to the server.
//! Account commands reuse the web app's own functions, so the rules are identical: temporary
//! passwords must be changed at sign-in, resets sign the user out everywhere, and names of
//! removed members stay reserved.
use crate::{
    auth,
    config::Config,
    db::{self, Verify},
    error::AppError,
    routes::users,
};
use rusqlite::{Connection, OptionalExtension};
use std::path::PathBuf;

pub const USAGE: &str = "\
Usage: homestock [--config PATH] [COMMAND]

Commands:
  serve                 Run the web app (the default)
  backup DEST           Write a verified copy of the database to DEST, which must not exist
  schema-version        Print the database schema version this build supports
  database-version      Print the schema version of the configured database
  list-users            List household members
  reset-password NAME   Give NAME a temporary password and sign them out everywhere
  create-admin NAME     Create an admin account with a temporary password
  make-admin NAME       Give an existing member the admin role

Options:
  --config PATH         Config file (default: ./config.toml)
  -h, --help            Show this help
";

#[derive(Debug, PartialEq)]
pub enum Command {
    Serve,
    Help,
    SchemaVersion,
    DatabaseVersion,
    Backup(PathBuf),
    ListUsers,
    ResetPassword(String),
    CreateAdmin(String),
    MakeAdmin(String),
}

#[derive(Debug, PartialEq)]
pub struct Invocation {
    pub config: PathBuf,
    pub command: Command,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Invocation, String> {
    let mut config = PathBuf::from("config.toml");
    let mut words = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = args.next().ok_or("--config needs a path")?.into(),
            "-h" | "--help" => {
                return Ok(Invocation {
                    config,
                    command: Command::Help,
                })
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            _ => words.push(arg),
        }
    }
    let mut words = words.into_iter();
    let mut operand = |what: &str| words.next().ok_or_else(|| format!("{what} is missing"));
    let command = match operand("command").ok().as_deref() {
        None | Some("serve") => Command::Serve,
        Some("help") => Command::Help,
        Some("schema-version") => Command::SchemaVersion,
        Some("database-version") => Command::DatabaseVersion,
        Some("backup") => Command::Backup(operand("backup DEST")?.into()),
        Some("list-users") => Command::ListUsers,
        Some("reset-password") => Command::ResetPassword(operand("reset-password NAME")?),
        Some("create-admin") => Command::CreateAdmin(operand("create-admin NAME")?),
        Some("make-admin") => Command::MakeAdmin(operand("make-admin NAME")?),
        Some(other) => return Err(format!("unknown command {other}")),
    };
    if let Some(extra) = words.next() {
        return Err(format!("unexpected argument {extra}"));
    }
    Ok(Invocation { config, command })
}

/// Run a command that works on the configured database; returns the text to print.
pub fn run(cfg: &Config, command: Command) -> Result<String, AppError> {
    match command {
        Command::DatabaseVersion => Ok(db::schema_version(&open_existing(cfg)?)?.to_string()),
        Command::Backup(dest) => {
            db::backup_to(&open_existing(cfg)?, &dest, Verify::Full)?;
            Ok(format!("Backed up and verified: {}", dest.display()))
        }
        Command::ListUsers => list_users(&open_current(cfg)?),
        Command::ResetPassword(name) => reset_password(&mut open_current(cfg)?, &name),
        Command::CreateAdmin(name) => create_admin(&mut open_current(cfg)?, &name),
        Command::MakeAdmin(name) => make_admin(&open_current(cfg)?, &name),
        Command::Serve | Command::Help | Command::SchemaVersion => {
            Err(AppError::internal("not a database command"))
        }
    }
}

/// The configured database, without migrating it: a backup must copy it as it is.
fn open_existing(cfg: &Config) -> Result<Connection, AppError> {
    let path = cfg.database_path();
    if !path.exists() {
        return Err(AppError::bad(format!("no database at {}", path.display())));
    }
    db::open_conn(&path)
}

/// The configured database, only if its schema is exactly what this build expects.
fn open_current(cfg: &Config) -> Result<Connection, AppError> {
    let conn = open_existing(cfg)?;
    match db::schema_version(&conn)? {
        v if v > db::CURRENT_SCHEMA_VERSION => Err(db::newer_schema(v)),
        v if v < db::CURRENT_SCHEMA_VERSION => Err(AppError::bad(format!(
            "the database is at schema version {v}; start Homestock once to migrate it to {}",
            db::CURRENT_SCHEMA_VERSION
        ))),
        _ => Ok(conn),
    }
}

fn list_users(c: &Connection) -> Result<String, AppError> {
    let mut stmt = c.prepare(
        "SELECT name, role, must_change_pw FROM user WHERE deleted_at IS NULL ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() {
        return Ok(
            "No household members yet. Open the app to run setup, or use create-admin.".into(),
        );
    }
    let width = rows
        .iter()
        .map(|(name, ..)| name.chars().count())
        .max()
        .unwrap_or(0)
        .max(4);
    let mut out = format!("{:width$}  ROLE    PASSWORD", "NAME");
    for (name, role, must_change) in rows {
        let password = if must_change {
            "must change at sign-in"
        } else {
            "set"
        };
        out.push_str(&format!("\n{name:width$}  {role:6}  {password}"));
    }
    Ok(out)
}

/// An active member by name, ignoring case; the error lists who exists.
fn find_member(c: &Connection, name: &str) -> Result<(i64, String, String), AppError> {
    let found = c
        .query_row(
            "SELECT id, name, role FROM user WHERE name = ?1 COLLATE NOCASE AND deleted_at IS NULL",
            [name.trim()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    found.ok_or_else(|| {
        let names = c
            .prepare("SELECT name FROM user WHERE deleted_at IS NULL ORDER BY name COLLATE NOCASE")
            .and_then(|mut stmt| {
                stmt.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default();
        AppError::bad(format!(
            "no household member named \"{}\" (members: {})",
            name.trim(),
            if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            }
        ))
    })
}

fn temporary_password_notice(name: &str, password: &str) -> String {
    format!(
        "Temporary password for {name}: {password}\n\
         {name} must choose a new password at their next sign-in."
    )
}

fn reset_password(c: &mut Connection, name: &str) -> Result<String, AppError> {
    let (id, ..) = find_member(c, name)?;
    let password = auth::temp_password();
    let name = users::set_temporary_password(c, id, &auth::hash(&password)?)?;
    Ok(temporary_password_notice(&name, &password)
        + &format!("\nAll of {name}'s existing sessions were signed out."))
}

fn create_admin(c: &mut Connection, name: &str) -> Result<String, AppError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(AppError::bad("enter a name of 1 to 60 characters"));
    }
    let password = auth::temp_password();
    if !users::insert_user(c, name, "admin", &auth::hash(&password)?)? {
        return Err(AppError::conflict(format!(
            "the name \"{name}\" is taken (names of removed members stay reserved); \
             use reset-password or make-admin for an existing member, or choose another name"
        )));
    }
    Ok(format!("Created admin {name}.\n") + &temporary_password_notice(name, &password))
}

fn make_admin(c: &Connection, name: &str) -> Result<String, AppError> {
    let (id, name, role) = find_member(c, name)?;
    if role == "admin" {
        return Ok(format!("{name} is already an admin."));
    }
    c.execute("UPDATE user SET role = 'admin' WHERE id = ?1", [id])?;
    Ok(format!("{name} is now an admin."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn parses_commands_and_options() {
        let parsed = |words: &[&str]| parse(args(words)).map(|i| (i.config, i.command));
        assert_eq!(parsed(&[]), Ok(("config.toml".into(), Command::Serve)));
        assert_eq!(
            parsed(&["--config", "/etc/h.toml"]),
            Ok(("/etc/h.toml".into(), Command::Serve))
        );
        assert_eq!(
            parsed(&["--config", "c.toml", "reset-password", "Alex Smith"]),
            Ok(("c.toml".into(), Command::ResetPassword("Alex Smith".into())))
        );
        assert_eq!(
            parsed(&["backup", "/x.sqlite3", "--config", "c.toml"]).map(|p| p.1),
            Ok(Command::Backup("/x.sqlite3".into()))
        );
        assert_eq!(
            parsed(&["--help", "backup"]).map(|p| p.1),
            Ok(Command::Help)
        );
        assert!(parsed(&["reset-password"]).is_err());
        assert!(parsed(&["make-admin", "A", "B"]).is_err());
        assert!(parsed(&["frobnicate"]).is_err());
        assert!(parsed(&["--verbose"]).is_err());
        assert!(parsed(&["--config"]).is_err());
    }

    struct Fixture {
        dir: PathBuf,
        cfg: Config,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture() -> Fixture {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("homestock-cli-test-{suffix}"));
        fs::create_dir_all(&dir).expect("create test directory");
        let cfg: Config = toml::from_str(&format!(
            "bind = \"127.0.0.1:0\"\ndata_dir = \"{}\"\n[backup]\n",
            dir.display()
        ))
        .expect("parse config");
        let db = db::Db::open(&cfg.database_path()).expect("create database");
        drop(db);
        let c = db::open_conn(&cfg.database_path()).expect("open database");
        c.execute_batch(
            "INSERT INTO user(name,role,password_hash) VALUES ('Alex','admin','old-hash'),('Sam','member','hash');
             INSERT INTO session(token,user_id,expires_at) VALUES ('alex-phone',1,9999999999);",
        )
        .expect("insert users");
        Fixture { dir, cfg }
    }

    fn query<T: rusqlite::types::FromSql>(f: &Fixture, sql: &str) -> T {
        db::open_conn(&f.cfg.database_path())
            .expect("open")
            .query_row(sql, [], |r| r.get(0))
            .expect(sql)
    }

    #[test]
    fn reset_password_issues_a_working_temporary_password_and_signs_out() {
        let f = fixture();
        let out = run(&f.cfg, Command::ResetPassword("alex".into())).expect("reset");
        let password = out
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("Temporary password for Alex: "))
            .expect("password line");
        assert!(auth::verify(
            password,
            &query::<String>(&f, "SELECT password_hash FROM user WHERE name='Alex'")
        ));
        assert!(query::<bool>(
            &f,
            "SELECT must_change_pw FROM user WHERE name='Alex'"
        ));
        assert_eq!(query::<i64>(&f, "SELECT COUNT(*) FROM session"), 0);
        let error =
            run(&f.cfg, Command::ResetPassword("Taylor".into())).expect_err("unknown member");
        assert!(error.to_string().contains("members: Alex, Sam"));
    }

    #[test]
    fn create_admin_adds_an_admin_and_keeps_names_unique() {
        let f = fixture();
        let out = run(&f.cfg, Command::CreateAdmin(" Jordan ".into())).expect("create admin");
        assert!(out.starts_with("Created admin Jordan."));
        assert_eq!(
            query::<String>(&f, "SELECT role FROM user WHERE name='Jordan'"),
            "admin"
        );
        assert!(query::<bool>(
            &f,
            "SELECT must_change_pw FROM user WHERE name='Jordan'"
        ));
        assert!(matches!(
            run(&f.cfg, Command::CreateAdmin("SAM".into())),
            Err(AppError::Conflict(_))
        ));
        assert!(run(&f.cfg, Command::CreateAdmin("  ".into())).is_err());
    }

    #[test]
    fn make_admin_promotes_a_member_once() {
        let f = fixture();
        assert_eq!(
            run(&f.cfg, Command::MakeAdmin("sam".into())).expect("promote"),
            "Sam is now an admin."
        );
        assert_eq!(
            run(&f.cfg, Command::MakeAdmin("Sam".into())).expect("again"),
            "Sam is already an admin."
        );
    }

    #[test]
    fn list_users_shows_roles_and_password_state() {
        let f = fixture();
        run(&f.cfg, Command::ResetPassword("Sam".into())).expect("reset");
        let out = run(&f.cfg, Command::ListUsers).expect("list");
        assert_eq!(
            out,
            "NAME  ROLE    PASSWORD\nAlex  admin   set\nSam   member  must change at sign-in"
        );
    }

    #[test]
    fn backup_writes_a_verified_copy_and_never_overwrites() {
        let f = fixture();
        let dest = f.dir.join("copy.sqlite3");
        run(&f.cfg, Command::Backup(dest.clone())).expect("backup");
        let copy = Connection::open(&dest).expect("open copy");
        assert_eq!(
            copy.query_row("SELECT COUNT(*) FROM user", [], |r| r.get::<_, i64>(0))
                .expect("count"),
            2
        );
        assert!(!f.dir.join("copy.sqlite3.part").exists());
        assert!(matches!(
            run(&f.cfg, Command::Backup(dest)),
            Err(AppError::Conflict(_))
        ));
        assert_eq!(
            run(&f.cfg, Command::DatabaseVersion).expect("version"),
            db::CURRENT_SCHEMA_VERSION.to_string()
        );
    }

    #[test]
    fn account_commands_refuse_a_database_on_another_schema() {
        let f = fixture();
        db::open_conn(&f.cfg.database_path())
            .expect("open")
            .execute_batch(&format!(
                "PRAGMA user_version = {}",
                db::CURRENT_SCHEMA_VERSION + 1
            ))
            .expect("bump");
        assert!(run(&f.cfg, Command::ListUsers)
            .unwrap_err()
            .to_string()
            .contains("newer release"));
        assert!(
            db::Db::open(&f.cfg.database_path()).is_err(),
            "the server refuses too"
        );
        let backup = f.dir.join("newer.sqlite3");
        run(&f.cfg, Command::Backup(backup.clone())).expect("a backup still works on any schema");
    }
}
