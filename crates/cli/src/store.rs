//! The local SQLite file every received frame is saved to before it is acknowledged.
//!
//! Raw frames are the source of truth. Decoded streams can always be rebuilt from them,
//! so a better decoder later never needs the strap to resend anything.

use std::path::Path;

use anyhow::{Context, Result};
use healthstrap_core::command::EndToken;
use healthstrap_core::frame::Frame;
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS frames (
    id INTEGER PRIMARY KEY,
    device TEXT NOT NULL,
    received_at_ms INTEGER NOT NULL,
    packet_type INTEGER NOT NULL,
    bytes BLOB NOT NULL,
    UNIQUE (device, bytes)
);
CREATE TABLE IF NOT EXISTS chunks (
    id INTEGER PRIMARY KEY,
    device TEXT NOT NULL,
    committed_at_ms INTEGER NOT NULL,
    token BLOB NOT NULL,
    frame_count INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS upload_cursor (
    server TEXT PRIMARY KEY,
    last_frame_id INTEGER NOT NULL
);
";

/// An open store.
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens or creates the store at `path`. Every commit is flushed to disk before returning.
    pub fn open(path: &Path) -> Result<Self> {
        let connection =
            Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self { connection })
    }

    /// Saves a chunk's frames and its token in one transaction. Duplicate frames are skipped.
    /// Returns how many frames were new.
    pub fn commit_chunk(
        &mut self,
        device: &str,
        received_at_ms: i64,
        frames: &[Frame],
        token: Option<&EndToken>,
    ) -> Result<usize> {
        let transaction = self.connection.transaction()?;
        let mut inserted = 0;
        {
            let mut insert = transaction.prepare_cached(
                "INSERT OR IGNORE INTO frames (device, received_at_ms, packet_type, bytes)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for frame in frames {
                inserted += insert.execute(params![
                    device,
                    received_at_ms,
                    frame.packet_type(),
                    frame.bytes()
                ])?;
            }
        }
        if let Some(token) = token {
            transaction.execute(
                "INSERT INTO chunks (device, committed_at_ms, token, frame_count)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    device,
                    received_at_ms,
                    token.0.as_slice(),
                    i64::try_from(frames.len())?
                ],
            )?;
        }
        transaction.commit()?;
        Ok(inserted)
    }

    /// Frames with an id above `after`, oldest first, with their ids.
    pub fn frames_after(&self, after: i64) -> Result<Vec<(i64, Frame)>> {
        let mut query = self
            .connection
            .prepare("SELECT id, bytes FROM frames WHERE id > ?1 ORDER BY id")?;
        let rows = query.query_map([after], |row| Ok((row.get(0)?, row.get::<_, Vec<u8>>(1)?)))?;
        let mut frames = Vec::new();
        for row in rows {
            let (id, bytes) = row?;
            // Only checked frames are ever stored, so a failure here means the file was damaged.
            let frame = Frame::parse(bytes).map_err(|e| anyhow::anyhow!("frame {id}: {e:?}"))?;
            frames.push((id, frame));
        }
        Ok(frames)
    }

    /// The id of the last frame already uploaded to `server`, or 0.
    pub fn upload_cursor(&self, server: &str) -> Result<i64> {
        let cursor = self
            .connection
            .query_row(
                "SELECT last_frame_id FROM upload_cursor WHERE server = ?1",
                [server],
                |row| row.get(0),
            )
            .optional()?;
        Ok(cursor.unwrap_or(0))
    }

    /// Records that every frame up to `last_frame_id` reached `server`.
    pub fn set_upload_cursor(&self, server: &str, last_frame_id: i64) -> Result<()> {
        self.connection.execute(
            "INSERT INTO upload_cursor (server, last_frame_id) VALUES (?1, ?2)
             ON CONFLICT (server) DO UPDATE SET last_frame_id = excluded.last_frame_id",
            params![server, last_frame_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use healthstrap_core::frame::encode_command;

    fn frame(sequence: u8) -> Frame {
        Frame::parse(encode_command(sequence, 22, &[0])).unwrap()
    }

    #[test]
    fn skips_frames_it_already_has() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let token = EndToken([1; 8]);
        assert_eq!(
            store
                .commit_chunk("strap", 0, &[frame(1), frame(2)], Some(&token))
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .commit_chunk("strap", 1, &[frame(2), frame(3)], Some(&token))
                .unwrap(),
            1
        );
        assert_eq!(
            store.commit_chunk("other", 1, &[frame(2)], None).unwrap(),
            1
        );
    }

    #[test]
    fn tracks_upload_progress_per_server() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        store
            .commit_chunk("strap", 0, &[frame(1), frame(2), frame(3)], None)
            .unwrap();
        assert_eq!(store.upload_cursor("home").unwrap(), 0);
        store.set_upload_cursor("home", 2).unwrap();
        assert_eq!(store.upload_cursor("home").unwrap(), 2);
        assert_eq!(store.upload_cursor("other").unwrap(), 0);
        let ids: Vec<i64> = store
            .frames_after(2)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids, vec![3]);
    }
}
