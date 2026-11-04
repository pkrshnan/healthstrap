//! The v1 sample API's wire format. CSV remains the local export format.

use anyhow::{Context, Result, bail};
use healthstrap_core::export::{Row, Stream};
use serde::Serialize;

/// A single UTC day's samples, transposed into the API's typed columns.
#[derive(Debug, Serialize)]
pub(super) struct MergeSamplesRequest {
    stream: String,
    day: UtcDate,
    // Protobuf JSON represents int64 values as decimal strings.
    timestamps: Vec<String>,
    columns: Vec<Column>,
}

impl MergeSamplesRequest {
    pub(super) fn new(stream: Stream, day: &str, rows: &[Row]) -> Result<Self> {
        let columns = stream
            .header()
            .split(',')
            .skip(1) // The first CSV column is the timestamp.
            .enumerate()
            .map(|(index, name)| Column::from_rows(name, index, rows))
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            stream: format!("streams/{}", stream.name()),
            day: UtcDate::parse(day)?,
            timestamps: rows.iter().map(|row| row.ts_ms.to_string()).collect(),
            columns,
        })
    }
}

#[derive(Debug, Serialize)]
struct UtcDate {
    year: u32,
    month: u32,
    day: u32,
}

impl UtcDate {
    fn parse(day: &str) -> Result<Self> {
        let parts = day
            .split('-')
            .map(str::parse)
            .collect::<Result<Vec<u32>, _>>()?;
        let [year, month, day]: [u32; 3] = parts
            .try_into()
            .map_err(|_| anyhow::anyhow!("UTC date must be YYYY-MM-DD"))?;
        Ok(Self { year, month, day })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Column {
    name: String,
    missing_rows: Vec<usize>,
    #[serde(flatten)]
    values: ColumnValues,
}

impl Column {
    fn from_rows(name: &str, index: usize, rows: &[Row]) -> Result<Self> {
        let mut values = ColumnValues::for_column(name);
        let mut missing_rows = Vec::new();
        for (row_index, row) in rows.iter().enumerate() {
            let cell = row
                .values
                .split(',')
                .nth(index)
                .context("missing export column")?;
            if cell.is_empty() {
                missing_rows.push(row_index);
            }
            values
                .push(cell)
                .with_context(|| format!("encoding column {name}, row {row_index}"))?;
        }
        Ok(Self {
            name: name.to_owned(),
            missing_rows,
            values,
        })
    }
}

/// Flattening this enum produces exactly one of `numbers` or `texts` in a column.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum ColumnValues {
    Numbers { values: Vec<f64> },
    Texts { values: Vec<String> },
}

impl ColumnValues {
    fn for_column(name: &str) -> Self {
        match name {
            "state" | "source" | "kind" => Self::Texts { values: Vec::new() },
            _ => Self::Numbers { values: Vec::new() },
        }
    }

    fn push(&mut self, cell: &str) -> Result<()> {
        match self {
            Self::Texts { values } => values.push(cell.to_owned()),
            Self::Numbers { values } => {
                // A missing row still needs a placeholder to keep every column aligned.
                let value: f64 = if cell.is_empty() { 0.0 } else { cell.parse()? };
                if !value.is_finite() {
                    bail!("sample number must be finite");
                }
                values.push(value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encodes_numbers_and_missing_rows() {
        let rows = vec![
            Row {
                ts_ms: 1_793_664_000_000,
                values: "75,1".to_owned(),
            },
            Row {
                ts_ms: 1_793_664_001_000,
                values: ",0".to_owned(),
            },
        ];
        let request = MergeSamplesRequest::new(Stream::Battery, "2026-11-03", &rows).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "stream": "streams/battery",
                "day": {"year": 2026, "month": 11, "day": 3},
                "timestamps": ["1793664000000", "1793664001000"],
                "columns": [
                    {"name": "pct", "missingRows": [1], "numbers": {"values": [75.0, 0.0]}},
                    {"name": "charging", "missingRows": [], "numbers": {"values": [1.0, 0.0]}}
                ]
            })
        );
    }

    #[test]
    fn keeps_text_values_as_text() {
        let rows = vec![Row {
            ts_ms: 1_793_664_000_000,
            values: "SLEEP".to_owned(),
        }];
        let request = MergeSamplesRequest::new(Stream::SleepState, "2026-11-03", &rows).unwrap();
        assert_eq!(
            serde_json::to_value(&request.columns).unwrap(),
            json!([
                {"name": "state", "missingRows": [], "texts": {"values": ["SLEEP"]}}
            ])
        );
    }
}
