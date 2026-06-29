//! SQLCipher-encrypted SQLite (records, notes) and migrations. Design §9.2. B2.
//!
//! All PHI lives here: encounters (`records`) and their generated SOAP notes
//! (`notes`). The connection is keyed with the raw AES-256 key from `crypto`
//! before any other statement runs, so the file on disk is encrypted at rest.

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use rusqlite_migration::{Migrations, M};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

/// A recorded encounter: the finalized, editable transcript. No audio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub label: String,
    pub language: String,
    pub created_at: i64,
    pub transcript: String,
}

/// Lightweight row for the saved-encounter list (FR-13); omits the transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordSummary {
    pub id: String,
    pub label: String,
    pub language: String,
    pub created_at: i64,
}

/// A generated SOAP note. Many per record; exactly one is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub record_id: String,
    pub soap_data: String,
    pub created_at: i64,
    pub is_active: bool,
}

/// Owns the keyed SQLCipher connection.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (or creates) the encrypted DB at `path`, applies `key`, and runs
    /// migrations. A wrong key fails here when the schema can't be decrypted.
    pub fn open(path: &Path, key: &[u8; crate::crypto::KEY_LEN]) -> Result<Self> {
        let mut conn = Connection::open(path).context("open SQLCipher database")?;

        // The key PRAGMA must be the first statement on the connection. Use the
        // raw-hex form so SQLCipher takes the bytes verbatim (no KDF over them).
        let hex = hex_encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
            .context("apply SQLCipher key")?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .context("enable foreign keys")?;

        // Touch the schema so a wrong key is rejected before we touch data.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        })
        .context("decrypt database (wrong key?)")?;

        migrations()
            .to_latest(&mut conn)
            .context("apply database migrations")?;

        Ok(Self { conn })
    }

    // --- Records ---------------------------------------------------------

    /// Inserts a new encounter and returns it.
    pub fn create_record(&self, label: &str, language: &str, transcript: &str) -> Result<Record> {
        let record = Record {
            id: new_id(),
            label: label.to_string(),
            language: language.to_string(),
            created_at: now(),
            transcript: transcript.to_string(),
        };
        self.conn
            .execute(
                "INSERT INTO records (id, label, language, created_at, transcript)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    record.id,
                    record.label,
                    record.language,
                    record.created_at,
                    record.transcript
                ],
            )
            .context("insert record")?;
        Ok(record)
    }

    /// Overwrites the transcript of a record (autosave, NFR-8).
    pub fn update_transcript(&self, id: &str, transcript: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE records SET transcript = ?2 WHERE id = ?1",
                params![id, transcript],
            )
            .context("update transcript")?;
        Ok(())
    }

    /// Lists saved encounters, newest first.
    pub fn list_records(&self) -> Result<Vec<RecordSummary>> {
        let mut stmt = self.conn.prepare(
            // rowid tiebreak keeps ordering stable for same-second inserts.
            "SELECT id, label, language, created_at FROM records
             ORDER BY created_at DESC, rowid DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RecordSummary {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    language: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("list records")?;
        Ok(rows)
    }

    /// Loads a full record by id, if it exists.
    pub fn open_record(&self, id: &str) -> Result<Option<Record>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, language, created_at, transcript FROM records WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(Record {
                id: row.get(0)?,
                label: row.get(1)?,
                language: row.get(2)?,
                created_at: row.get(3)?,
                transcript: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r.context("read record")?)),
            None => Ok(None),
        }
    }

    /// Permanently deletes a record and its notes (NFR-9). Cascade via FK.
    pub fn delete_record(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM records WHERE id = ?1", params![id])
            .context("delete record")?;
        Ok(())
    }

    // --- Notes -----------------------------------------------------------

    /// Inserts a new note for a record and makes it the active version (§8.5).
    pub fn insert_note(&self, record_id: &str, soap_data: &str) -> Result<Note> {
        let note = Note {
            id: new_id(),
            record_id: record_id.to_string(),
            soap_data: soap_data.to_string(),
            created_at: now(),
            is_active: true,
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE notes SET is_active = 0 WHERE record_id = ?1",
            params![record_id],
        )?;
        tx.execute(
            "INSERT INTO notes (id, record_id, soap_data, created_at, is_active)
             VALUES (?1, ?2, ?3, ?4, 1)",
            params![note.id, note.record_id, note.soap_data, note.created_at],
        )?;
        tx.commit().context("insert note")?;
        Ok(note)
    }

    /// Lists all notes for a record, newest first.
    pub fn list_notes(&self, record_id: &str) -> Result<Vec<Note>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, record_id, soap_data, created_at, is_active
             FROM notes WHERE record_id = ?1
             ORDER BY created_at DESC, rowid DESC",
        )?;
        let rows = stmt
            .query_map(params![record_id], |row| {
                Ok(Note {
                    id: row.get(0)?,
                    record_id: row.get(1)?,
                    soap_data: row.get(2)?,
                    created_at: row.get(3)?,
                    is_active: row.get::<_, i64>(4)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("list notes")?;
        Ok(rows)
    }

    /// The record's current active note version, if any (§8.5). The EMR hand-off
    /// (§8.6) always pastes from this, so it reflects the latest edit/regeneration.
    pub fn active_note(&self, record_id: &str) -> Result<Option<Note>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, record_id, soap_data, created_at, is_active
             FROM notes WHERE record_id = ?1 AND is_active = 1",
        )?;
        let mut rows = stmt.query_map(params![record_id], |row| {
            Ok(Note {
                id: row.get(0)?,
                record_id: row.get(1)?,
                soap_data: row.get(2)?,
                created_at: row.get(3)?,
                is_active: row.get::<_, i64>(4)? != 0,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r.context("read active note")?)),
            None => Ok(None),
        }
    }

    /// Autosaves the clinician's in-place edits to a note (§8.5). Edits refine the
    /// active version rather than spawning a new one; a fresh version is created
    /// only by an explicit (re)generation via [`insert_note`].
    pub fn update_note(&self, id: &str, soap_data: &str) -> Result<()> {
        let updated = self
            .conn
            .execute(
                "UPDATE notes SET soap_data = ?2 WHERE id = ?1",
                params![id, soap_data],
            )
            .context("update note")?;
        // A no-op UPDATE returns Ok(0); without this an autosave to a stale/unknown
        // id would report success while the clinician's edit was silently dropped.
        if updated == 0 {
            return Err(anyhow!("note {id} not found"));
        }
        Ok(())
    }

    /// Flips the active version for a record (revert, §8.5).
    pub fn set_active_note(&self, record_id: &str, note_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE notes SET is_active = 0 WHERE record_id = ?1",
            params![record_id],
        )?;
        let activated = tx.execute(
            "UPDATE notes SET is_active = 1 WHERE id = ?1 AND record_id = ?2",
            params![note_id, record_id],
        )?;
        // Guard the "exactly one active per record" invariant (§9.2): a stale or
        // foreign note_id must fail and roll back, never leave zero active.
        if activated != 1 {
            return Err(anyhow!("note {note_id} not found for record {record_id}"));
        }
        tx.commit().context("set active note")?;
        Ok(())
    }
}

/// Thread-safe shared handle to the encrypted store. `rusqlite::Connection` is
/// `Send` but not `Sync`, so access is serialized behind a `Mutex`; clones share
/// the one keyed connection — the pipeline persists a record on stop while the
/// records commands read/write the same DB (managed as Tauri state, B8).
#[derive(Clone)]
pub struct SharedStore(Arc<Mutex<Store>>);

impl SharedStore {
    pub fn new(store: Store) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    /// Locks the connection, recovering a poisoned lock rather than wedging the
    /// app (a panic mid-statement leaves the DB readable).
    pub fn lock(&self) -> MutexGuard<'_, Store> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Schema migrations (design §9.2). Append new `M::up(...)` entries; never edit
/// a released one.
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(
        "CREATE TABLE records (
            id          TEXT PRIMARY KEY,
            label       TEXT NOT NULL,
            language    TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            transcript  TEXT NOT NULL
        );
        CREATE TABLE notes (
            id          TEXT PRIMARY KEY,
            record_id   TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
            soap_data   TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            is_active   INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX idx_notes_record ON notes(record_id);",
    )])
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(seed: u8) -> [u8; crate::crypto::KEY_LEN] {
        [seed; crate::crypto::KEY_LEN]
    }

    #[test]
    fn migrations_are_valid_and_idempotent() {
        // rusqlite_migration's own validity check plus a re-open on the same DB.
        assert!(migrations().validate().is_ok());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        let key = test_key(1);
        Store::open(&path, &key).unwrap();
        // Re-opening applies migrations again; must be a no-op.
        Store::open(&path, &key).unwrap();
    }

    #[test]
    fn record_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("c.db"), &test_key(2)).unwrap();

        let rec = store.create_record("Visit A", "en", "hello").unwrap();
        let loaded = store.open_record(&rec.id).unwrap().unwrap();
        assert_eq!(loaded.label, "Visit A");
        assert_eq!(loaded.transcript, "hello");

        store.update_transcript(&rec.id, "edited").unwrap();
        assert_eq!(store.open_record(&rec.id).unwrap().unwrap().transcript, "edited");

        assert_eq!(store.list_records().unwrap().len(), 1);
        store.delete_record(&rec.id).unwrap();
        assert!(store.open_record(&rec.id).unwrap().is_none());
    }

    #[test]
    fn notes_track_single_active_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("c.db"), &test_key(3)).unwrap();
        let rec = store.create_record("V", "en", "t").unwrap();

        let n1 = store.insert_note(&rec.id, "## S\n1").unwrap();
        let n2 = store.insert_note(&rec.id, "## S\n2").unwrap();

        let notes = store.list_notes(&rec.id).unwrap();
        assert_eq!(notes.len(), 2);
        // Exactly one active, and it's the newest insert.
        assert_eq!(notes.iter().filter(|n| n.is_active).count(), 1);
        assert!(notes.iter().find(|n| n.id == n2.id).unwrap().is_active);

        // An in-place edit autosaves onto a version without spawning a new one.
        store.update_note(&n2.id, "## S\n2 edited").unwrap();
        let notes = store.list_notes(&rec.id).unwrap();
        assert_eq!(notes.len(), 2, "editing must not create a version");
        assert_eq!(
            notes.iter().find(|n| n.id == n2.id).unwrap().soap_data,
            "## S\n2 edited"
        );
        // A mistargeted autosave must error, not silently drop the edit.
        assert!(store.update_note("nope", "## S\nlost").is_err());

        store.set_active_note(&rec.id, &n1.id).unwrap();
        let notes = store.list_notes(&rec.id).unwrap();
        assert!(notes.iter().find(|n| n.id == n1.id).unwrap().is_active);
        assert!(!notes.iter().find(|n| n.id == n2.id).unwrap().is_active);
        // active_note tracks the flip — hand-off (§8.6) pastes from this.
        assert_eq!(store.active_note(&rec.id).unwrap().unwrap().id, n1.id);
        assert!(store.active_note("ghost").unwrap().is_none());

        // A bogus note_id must error and leave the prior active note intact.
        assert!(store.set_active_note(&rec.id, "nope").is_err());
        let notes = store.list_notes(&rec.id).unwrap();
        assert_eq!(notes.iter().filter(|n| n.is_active).count(), 1);
        assert!(notes.iter().find(|n| n.id == n1.id).unwrap().is_active);

        // Deleting the record cascades to its notes.
        store.delete_record(&rec.id).unwrap();
        assert!(store.list_notes(&rec.id).unwrap().is_empty());
    }

    #[test]
    fn delete_record_cascades_to_notes() {
        // Focused guard for the FK cascade (§9.2): a record with notes, deleted,
        // must leave no orphaned notes. Catches `PRAGMA foreign_keys=ON` ever being
        // dropped — without it the notes would persist with a dangling record_id.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("c.db"), &test_key(6)).unwrap();

        let rec = store.create_record("V", "en", "t").unwrap();
        store.insert_note(&rec.id, "## S\n note").unwrap();
        assert_eq!(store.list_notes(&rec.id).unwrap().len(), 1);

        store.delete_record(&rec.id).unwrap();
        assert!(store.list_notes(&rec.id).unwrap().is_empty());
    }

    #[test]
    fn wrong_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        {
            let store = Store::open(&path, &test_key(4)).unwrap();
            store.create_record("V", "en", "secret").unwrap();
        }
        // A different key must fail to decrypt the existing DB.
        assert!(Store::open(&path, &test_key(5)).is_err());
    }
}
