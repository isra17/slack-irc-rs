use std;
use std::path::Path;
use rusqlite::{self, Connection};

type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct UserInfo {
    pub id: u32,
    pub workspace: String,
    pub nick: String,
    pub pass: String,
    pub token: Option<String>,
}

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Sqlite(e)
    }
}

impl std::error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::Sqlite(ref e) => e.description(),
        }
    }

    fn cause(&self) -> Option<&std::error::Error> {
        match *self {
            Error::Sqlite(ref e) => Some(e),
        }
    }
}

pub struct Datastore {
    conn: Connection,
}

impl Datastore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Datastore> {
        let db = Datastore { conn: Connection::open(path)? };
        db.init()?;
        Ok(db)
    }

    pub fn find_user(&self, workspace: &str, nick: &str) -> Option<UserInfo> {
        self.conn
            .query_row(r#"
            SELECT id, workspace, nick, pass, token
            FROM users
            WHERE workspace = ? AND nick = ?"#,
                       &[&workspace, &nick],
                       |row| {
                UserInfo {
                    id: row.get("id"),
                    workspace: row.get("workspace"),
                    nick: row.get("nick"),
                    pass: row.get("pass"),
                    token: row.get("token"),
                }
            })
            .ok()
    }

    pub fn insert_user(&self, user: &UserInfo) -> Result<()> {
        self.conn
            .execute("INSERT INTO users (workspace, nick, pass, token)
                      \
                      VALUES (?, ?, ?, ?)",
                     &[&user.workspace, &user.nick, &user.pass, &user.token])
            .map_err(Error::Sqlite)
            .map(|_| ())
    }

    pub fn update_token(&self, user: &UserInfo) -> Result<()> {
        self.conn
            .execute(r#"
                UPDATE users
                SET token = ?
                WHERE id = ? "#,
                     &[&user.token, &user.id])
            .map_err(Error::Sqlite)
            .map(|_| ())
    }

    fn init(&self) -> Result<()> {
        self.conn
            .execute_batch(r#"
        BEGIN;
        CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY,
            workspace TEXT NOT NULL,
            nick TEXT NOT NULL,
            pass TEXT NOT NULL,
            token TEXT NULL,
            UNIQUE (workspace, nick)
        );
        COMMIT;"#)
            .map_err(Error::Sqlite)
    }
}
