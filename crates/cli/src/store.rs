//! The local SQLite file every received frame is saved to before it is acknowledged.
//!
//! Raw frames are the source of truth. Decoded streams can always be rebuilt from them,
//! so a better decoder later never needs the strap to resend anything.

use std::path::Path;

use anyhow::{Context, Result};
use healthstrap_core::command::EndToken;
use healthstrap_core::frame::Frame;
use rusqlite::{Connection, params};

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
}
