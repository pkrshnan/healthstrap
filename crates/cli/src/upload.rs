//! Sends new rows to the command-center ingest API, and writes CSVs for inspection.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use healthstrap_core::export::{self, Batches};

use crate::store::Store;

mod samples;
use samples::MergeSamplesRequest;

/// What one upload sent.
#[derive(Debug, Default)]
pub struct Summary {
    /// Files posted, one per stream per day.
    pub files: usize,
    /// Rows across all files.
    pub rows: usize,
}

/// Posts rows from every frame not yet uploaded to `server`, then moves the cursor.
///
/// The cursor only moves after every file succeeds. A failed run sends the same rows again
/// next time, which is safe because the server merges by `ts`.
pub fn upload(store: &Store, server: &str, token: &str) -> Result<Summary> {
    let after = store.upload_cursor(server)?;
    let frames = store.frames_after(after)?;
    let Some(&(last_id, _)) = frames.last() else {
        return Ok(Summary::default());
    };

    let batches = export::batch(frames.iter().map(|(_, frame)| frame));
    let mut summary = Summary::default();
    for ((stream, day), rows) in &batches {
        let request = MergeSamplesRequest::new(*stream, day, rows)?;
        let body = serde_json::to_vec(&request)?;
        let url = format!(
            "{}/v1/streams/{}:mergeSamples",
            server.trim_end_matches('/'),
            stream.name()
        );
        ureq::post(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .send(body.as_slice())
            .with_context(|| format!("uploading {} for {day}", stream.name()))?;
        summary.files += 1;
        summary.rows += rows.len();
    }
    store.set_upload_cursor(server, last_id)?;
    Ok(summary)
}

/// Writes every stored record as `<dir>/<stream>/<day>.csv`, for looking at by hand.
pub fn export_all(store: &Store, dir: &Path) -> Result<usize> {
    let frames = store.frames_after(0)?;
    let batches: Batches = export::batch(frames.iter().map(|(_, frame)| frame));
    for ((stream, day), rows) in &batches {
        let stream_dir = dir.join(stream.name());
        fs::create_dir_all(&stream_dir)?;
        let path = stream_dir.join(format!("{day}.csv"));
        fs::write(&path, export::to_csv(*stream, rows))
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(batches.len())
}
