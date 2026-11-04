//! Sends new rows to the command-center ingest API, and writes CSVs for inspection.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use healthstrap_core::export::{self, Batches};

use crate::store::Store;

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
        let body = merge_body(*stream, day, rows)?.to_string();
        let url = format!(
            "{}/v1/streams/{}:mergeSamples",
            server.trim_end_matches('/'),
            stream.name()
        );
        ureq::post(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .send(body.as_bytes())
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

/// Protobuf JSON uses decimal strings for int64 timestamps and explicit missing-row indexes.
fn merge_body(
    stream: export::Stream,
    day: &str,
    rows: &[export::Row],
) -> Result<serde_json::Value> {
    let parts: Vec<u32> = day
        .split('-')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let [year, month, date]: [u32; 3] = parts
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid UTC day"))?;
    let names: Vec<&str> = stream.header().split(',').skip(1).collect();
    let cells: Vec<Vec<&str>> = rows.iter().map(|r| r.values.split(',').collect()).collect();
    let mut columns = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let is_text = matches!(*name, "state" | "source" | "kind");
        let mut missing = Vec::new();
        let mut values = Vec::new();
        for (row_index, row) in cells.iter().enumerate() {
            let cell = row.get(index).context("missing export column")?;
            if cell.is_empty() {
                missing.push(row_index);
            }
            values.push(if is_text {
                serde_json::json!(cell)
            } else {
                serde_json::json!(if cell.is_empty() {
                    0.0
                } else {
                    cell.parse::<f64>()?
                })
            });
        }
        let mut column = serde_json::json!({"name":name,"missingRows":missing});
        column
            .as_object_mut()
            .context("column must be object")?
            .insert(
                if is_text { "texts" } else { "numbers" }.to_owned(),
                serde_json::json!({"values":values}),
            );
        columns.push(column);
    }
    Ok(serde_json::json!({
        "stream":format!("streams/{}",stream.name()),
        "day":{"year":year,"month":month,"day":date},
        "timestamps":rows.iter().map(|r|r.ts_ms.to_string()).collect::<Vec<_>>(),
        "columns":columns,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_protobuf_json_columns_with_explicit_missing_rows() {
        let rows = vec![
            export::Row {
                ts_ms: 1_793_664_000_000,
                values: "75,1".to_owned(),
            },
            export::Row {
                ts_ms: 1_793_664_001_000,
                values: ",0".to_owned(),
            },
        ];
        let body = merge_body(export::Stream::Battery, "2026-11-03", &rows).unwrap();
        assert_eq!(body.get("stream").unwrap(), "streams/battery");
        assert_eq!(
            body.get("timestamps").unwrap(),
            &serde_json::json!(["1793664000000", "1793664001000"])
        );
        let columns = body.get("columns").unwrap().as_array().unwrap();
        assert_eq!(
            columns.first().unwrap().get("missingRows").unwrap(),
            &serde_json::json!([1])
        );
        assert_eq!(
            columns.first().unwrap().get("numbers").unwrap(),
            &serde_json::json!({"values":[75.0,0.0]})
        );
    }

    #[test]
    fn keeps_text_streams_as_text() {
        let rows = vec![export::Row {
            ts_ms: 1_793_664_000_000,
            values: "SLEEP".to_owned(),
        }];
        let body = merge_body(export::Stream::SleepState, "2026-11-03", &rows).unwrap();
        let columns = body.get("columns").unwrap().as_array().unwrap();
        assert_eq!(
            columns.first().unwrap().get("texts").unwrap(),
            &serde_json::json!({"values":["SLEEP"]})
        );
    }
}
