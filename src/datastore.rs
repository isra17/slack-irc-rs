use serde_json;
use std;
use std::path::Path;
use rusqlite::{self, Connection};
use hub::HubInfo;

type Result<T> = std::result::Result<T, Error>;

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

#[derive(Debug)]
pub struct Datastore {
    conn: Connection,
}

impl Datastore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Datastore> {
        let db = Datastore { conn: Connection::open(path)? };
        db.init()?;
        Ok(db)
    }

    pub fn find_hub(&self, workspace: &str, nick: &str) -> Option<HubInfo> {
        self.conn
            .query_row(r#"
            SELECT id, workspace, nick, settings
            FROM users
            WHERE workspace = ? AND nick = ?"#,
                       &[&workspace, &nick],
                       |row| {
                HubInfo {
                    id: row.get("id"),
                    workspace: row.get("workspace"),
                    nick: row.get("nick"),
                    settings: serde_json::from_str(&row.get::<_, String>("settings")).unwrap(),
                }
            })
            .ok()
    }

    pub fn insert_hub(&self, hub: &HubInfo) -> Result<()> {
        self.conn
            .execute("INSERT INTO users (workspace, nick, settings)
                      \
                      VALUES (?, ?, ?)",
                     &[&hub.workspace, &hub.nick, &serde_json::to_string(&hub.settings).unwrap()])
            .map_err(Error::Sqlite)
            .map(|_| ())
    }

    pub fn update_hub(&self, user: &HubInfo) -> Result<()> {
        self.conn
            .execute(r#"
                UPDATE users
                SET settings = ?
                WHERE id = ? "#,
                     &[&serde_json::to_string(&user.settings).unwrap(), &user.id])
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
            settings TEXT NOT NULL,
            UNIQUE (workspace, nick)
        );
        COMMIT;"#)
            .map_err(Error::Sqlite)
    }
}
