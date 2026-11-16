//! `hub.sqlite`: the paired devices and the pairing PIN's hash.
//!
//! A device's token is never stored: only its SHA-256 (the token is 32 random bytes, so a
//! plain hash is enough to make the stored value useless to a thief). Revoking a device
//! keeps its row (for the device list) and refuses its token from then on. The schema has
//! migrations like a book, so any newer hub opens an older registry without re-pairing.

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::util::StoreResult;

pub const REGISTRY_FILE: &str = "hub.sqlite";

const MIGRATIONS: &[&str] = &[
    // 0 → 1
    "CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE devices (
         id         INTEGER PRIMARY KEY,
         token_hash BLOB NOT NULL UNIQUE,
         book_id    TEXT NOT NULL,
         member     INTEGER NOT NULL,
         name       TEXT NOT NULL,
         created    INTEGER NOT NULL,
         last_seen  INTEGER,
         revoked    INTEGER
     );",
];

/// A paired device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: u32,
    pub book_id: String,
    pub member: u32,
    pub name: String,
    pub created: i64,
    pub last_seen: Option<i64>,
    pub revoked: Option<i64>,
}

#[derive(Debug)]
pub struct Registry {
    conn: Connection,
}

fn sql(e: rusqlite::Error) -> String {
    format!("the device list can't be used: {e}")
}

/// The stored form of a device token.
pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        id: r.get::<_, i64>(0)? as u32,
        book_id: r.get(1)?,
        member: r.get::<_, i64>(2)? as u32,
        name: r.get(3)?,
        created: r.get(4)?,
        last_seen: r.get(5)?,
        revoked: r.get(6)?,
    })
}

const COLUMNS: &str = "id, book_id, member, name, created, last_seen, revoked";

impl Registry {
    pub fn open(dir: &Path) -> StoreResult<Registry> {
        let mut conn = Connection::open(dir.join(REGISTRY_FILE)).map_err(sql)?;
        conn.pragma_update(None, "journal_mode", "DELETE")
            .map_err(sql)?;
        Self::migrate(&mut conn)?;
        Ok(Registry { conn })
    }

    pub fn in_memory() -> StoreResult<Registry> {
        let mut conn = Connection::open_in_memory().map_err(sql)?;
        Self::migrate(&mut conn)?;
        Ok(Registry { conn })
    }

    fn migrate(conn: &mut Connection) -> StoreResult<()> {
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(sql)?;
        let to = MIGRATIONS.len() as i64;
        if v > to {
            return Err(format!(
                "The device list was made by a newer ShareKeep hub (version {v}); update ShareKeep."
            ));
        }
        let tx = conn.transaction().map_err(sql)?;
        for m in MIGRATIONS.iter().skip(v as usize) {
            tx.execute_batch(m).map_err(sql)?;
        }
        tx.pragma_update(None, "user_version", to).map_err(sql)?;
        tx.commit().map_err(sql)
    }

    pub fn get(&self, key: &str) -> StoreResult<Option<String>> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .map_err(sql)
    }

    pub fn set(&self, key: &str, value: &str) -> StoreResult<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
                [key, value],
            )
            .map(|_| ())
            .map_err(sql)
    }

    /// Record a newly paired device; returns its id.
    pub fn add(
        &self,
        token: &str,
        book_id: &str,
        member: u32,
        name: &str,
        now: i64,
    ) -> StoreResult<u32> {
        self.conn
            .execute(
                "INSERT INTO devices (token_hash, book_id, member, name, created) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![token_hash(token), book_id, i64::from(member), name, now],
            )
            .map_err(sql)?;
        Ok(self.conn.last_insert_rowid() as u32)
    }

    /// The device holding `token`, unless it was revoked.
    pub fn by_token(&self, token: &str) -> StoreResult<Option<Device>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM devices WHERE token_hash = ?1 AND revoked IS NULL"),
                [token_hash(token)],
                row,
            )
            .optional()
            .map_err(sql)
    }

    pub fn touch(&self, id: u32, now: i64) -> StoreResult<()> {
        self.conn
            .execute(
                "UPDATE devices SET last_seen = ?2 WHERE id = ?1",
                rusqlite::params![i64::from(id), now],
            )
            .map(|_| ())
            .map_err(sql)
    }

    pub fn list(&self) -> StoreResult<Vec<Device>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM devices ORDER BY id"))
            .map_err(sql)?;
        let rows = stmt.query_map([], row).map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql)
    }

    /// Refuse the device's token from now on. `false` when there is no such device.
    pub fn revoke(&self, id: u32, now: i64) -> StoreResult<bool> {
        let n = self
            .conn
            .execute(
                "UPDATE devices SET revoked = ?2 WHERE id = ?1 AND revoked IS NULL",
                rusqlite::params![i64::from(id), now],
            )
            .map_err(sql)?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_are_found_by_token_until_revoked() {
        let r = Registry::in_memory().unwrap();
        let id = r.add("tok-1", "book", 2, "Sam's phone", 10).unwrap();
        assert_eq!(r.by_token("tok-1").unwrap().unwrap().member, 2);
        assert!(r.by_token("tok-2").unwrap().is_none());
        r.touch(id, 20).unwrap();
        assert_eq!(r.list().unwrap()[0].last_seen, Some(20));
        assert!(r.revoke(id, 30).unwrap());
        assert!(r.by_token("tok-1").unwrap().is_none());
        assert!(!r.revoke(id, 31).unwrap());
        assert_eq!(r.list().unwrap()[0].revoked, Some(30));
    }

    #[test]
    fn the_registry_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        {
            let r = Registry::open(dir.path()).unwrap();
            r.add("tok", "book", 1, "desk", 1).unwrap();
            r.set("pin", "hash").unwrap();
        }
        let r = Registry::open(dir.path()).unwrap();
        assert!(r.by_token("tok").unwrap().is_some());
        assert_eq!(r.get("pin").unwrap().as_deref(), Some("hash"));
    }

    #[test]
    fn a_newer_registry_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = Connection::open(dir.path().join(REGISTRY_FILE)).unwrap();
            c.pragma_update(None, "user_version", 99).unwrap();
        }
        assert!(Registry::open(dir.path()).is_err());
    }
}
