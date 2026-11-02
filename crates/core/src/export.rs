//! Turns decoded records into the ingest API's format: one CSV per stream per UTC day.
//!
//! Each CSV starts with a `ts` column in UTC epoch milliseconds, followed by value columns
//! with no units in their names. The server merges rows by `ts`, so sending only new rows
//! is enough.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::frame::Frame;
use crate::log::{self, Fact};
use crate::record::{BiometricSummary, OpticalWindow};

/// A stream the ingest API accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stream {
    /// Beats per minute, about once a second.
    HeartRate,
    /// Time between beats, in milliseconds.
    RrInterval,
    /// Gravity vector in g, about once a second.
    Accel,
    /// Skin temperature in °C.
    Temperature,
    /// Motion intensity and its longer-window companion, raw, about once a second.
    Motion,
    /// The strap's cumulative step counter, raw: it wraps and may reset.
    Steps,
    /// Two more temperature-like readings, raw.
    TemperatureAux,
    /// The strap's own on-body (`1`) and off-body (`0`) decisions, at each change.
    Wear,
    /// The strap's own sleep state (`WAKE`, `STILL`, `SLEEP`, `UP`), at each change.
    SleepState,
    /// Battery percent and whether it is charging; either may be empty when not yet known.
    Battery,
    /// Other strap events: `double_tap`, `spo2_computed`.
    Events,
    /// Raw optical samples at 25 Hz, during the strap's short bursts.
    Ppg,
}

impl Stream {
    /// Every stream, in a stable order.
    pub const ALL: [Self; 12] = [
        Self::HeartRate,
        Self::RrInterval,
        Self::Accel,
        Self::Temperature,
        Self::Motion,
        Self::Steps,
        Self::TemperatureAux,
        Self::Wear,
        Self::SleepState,
        Self::Battery,
        Self::Events,
        Self::Ppg,
    ];

    /// The name used in the upload path.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::HeartRate => "heart_rate",
            Self::RrInterval => "rr_interval",
            Self::Accel => "accel",
            Self::Temperature => "temperature",
            Self::Motion => "motion",
            Self::Steps => "steps",
            Self::TemperatureAux => "temperature_aux",
            Self::Wear => "wear",
            Self::SleepState => "sleep_state",
            Self::Battery => "battery",
            Self::Events => "events",
            Self::Ppg => "ppg",
        }
    }

    /// The CSV header line.
    #[must_use]
    pub const fn header(self) -> &'static str {
        match self {
            Self::HeartRate => "ts,bpm",
            Self::RrInterval => "ts,ms",
            Self::Accel => "ts,x,y,z",
            Self::Temperature => "ts,celsius",
            Self::Motion => "ts,intensity,intensity_long",
            Self::Steps => "ts,count",
            Self::TemperatureAux => "ts,t1,t2",
            Self::Wear => "ts,on,source",
            Self::SleepState => "ts,state",
            Self::Battery => "ts,pct,charging",
            Self::Events => "ts,kind",
            Self::Ppg => "ts,burst,value,accepted,motion",
        }
    }
}

/// One CSV row: a timestamp and its already-formatted values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// UTC epoch milliseconds.
    pub ts_ms: i64,
    /// Comma-separated values, matching the stream's header.
    pub values: String,
}

/// Rows grouped by stream and UTC day (`YYYY-MM-DD`), each group sorted by time.
pub type Batches = BTreeMap<(Stream, String), Vec<Row>>;

/// Decodes records and debug-log facts among `frames` and groups the rows for upload.
///
/// Frames must be in the order they were received, because log lines span packets.
/// Other frames are ignored.
pub fn batch<'a>(frames: impl IntoIterator<Item = &'a Frame>) -> Batches {
    let mut batches = Batches::new();
    let mut joiner = log::Joiner::new();
    let mut battery = BatteryState::default();
    for frame in frames {
        let mut found = BiometricSummary::parse(frame)
            .map(|summary| rows(&summary))
            .unwrap_or_default();
        found.extend(OpticalWindow::parse(frame).map_or_else(Vec::new, |w| ppg_rows(&w)));
        for line in joiner.push(frame) {
            if let Some(fact) = Fact::parse(&line.message) {
                let ts_ms = i64::from(line.unix_seconds) * 1000;
                found.push(battery.fact_row(ts_ms, &fact));
            }
        }
        for (stream, row) in found {
            let day = utc_date(row.ts_ms.div_euclid(1000));
            batches.entry((stream, day)).or_default().push(row);
        }
    }
    for rows in batches.values_mut() {
        rows.sort_by_key(|row| row.ts_ms);
        rows.dedup_by_key(|row| row.ts_ms);
    }
    batches
}

/// The last battery percent and charging state seen, so each battery row carries both.
#[derive(Debug, Default)]
struct BatteryState {
    percent: Option<f32>,
    charging: Option<bool>,
}

impl BatteryState {
    /// The row a log fact contributes, updating the battery state on the way.
    fn fact_row(&mut self, ts_ms: i64, fact: &Fact) -> (Stream, Row) {
        let (stream, values) = match fact {
            Fact::Wear(on) => (Stream::Wear, format!("{},debug_log", flag(*on))),
            Fact::SleepState(state) => (Stream::SleepState, state.clone()),
            Fact::Battery(percent) => {
                self.percent = Some(*percent);
                (Stream::Battery, self.values())
            }
            Fact::Charging(charging) => {
                self.charging = Some(*charging);
                (Stream::Battery, self.values())
            }
            Fact::DoubleTap => (Stream::Events, "double_tap".to_owned()),
            Fact::SpO2Computed => (Stream::Events, "spo2_computed".to_owned()),
        };
        (stream, Row { ts_ms, values })
    }

    fn values(&self) -> String {
        let percent = self.percent.map(|p| format!("{p:.1}")).unwrap_or_default();
        let charging = self.charging.map(flag).unwrap_or_default();
        format!("{percent},{charging}")
    }
}

const fn flag(on: bool) -> &'static str {
    if on { "1" } else { "0" }
}

/// Renders one batch as CSV text, header first.
#[must_use]
pub fn to_csv(stream: Stream, rows: &[Row]) -> String {
    let mut csv = String::from(stream.header());
    csv.push('\n');
    for row in rows {
        let _ = writeln!(csv, "{},{}", row.ts_ms, row.values);
    }
    csv
}

/// The rows one summary contributes to each stream.
fn rows(summary: &BiometricSummary) -> Vec<(Stream, Row)> {
    let ts_ms = i64::from(summary.unix_seconds) * 1000;
    let mut rows = Vec::new();
    if summary.heart_rate > 0 {
        rows.push((
            Stream::HeartRate,
            Row {
                ts_ms,
                values: summary.heart_rate.to_string(),
            },
        ));
    }
    rows.extend(rr_rows(ts_ms, &summary.rr_intervals_ms).map(|row| (Stream::RrInterval, row)));
    let [x, y, z] = summary.gravity;
    if summary.gravity.iter().all(|value| value.is_finite()) {
        rows.push((
            Stream::Accel,
            Row {
                ts_ms,
                values: format!("{x:.4},{y:.4},{z:.4}"),
            },
        ));
    }
    if let Some(celsius) = summary.skin_temperature_c {
        rows.push((
            Stream::Temperature,
            Row {
                ts_ms,
                values: format!("{celsius:.2}"),
            },
        ));
    }
    rows.extend(raw_rows(ts_ms, summary));
    rows
}

/// Rows for fields whose meaning is inferred, not confirmed. Values are passed on raw.
fn raw_rows(ts_ms: i64, summary: &BiometricSummary) -> Vec<(Stream, Row)> {
    let row = |values: String| Row { ts_ms, values };
    let mut rows = Vec::new();
    if summary.motion.is_finite() && summary.motion_long.is_finite() {
        rows.push((
            Stream::Motion,
            row(format!("{},{}", summary.motion, summary.motion_long)),
        ));
    }
    let [t1, t2] = summary.temperature_aux;
    rows.push((Stream::Steps, row(summary.step_counter.to_string())));
    rows.push((Stream::TemperatureAux, row(format!("{t1},{t2}"))));
    rows
}

/// One row per optical sample, 40 ms apart from the window's second.
fn ppg_rows(window: &OpticalWindow) -> Vec<(Stream, Row)> {
    let start_ms = i64::from(window.unix_seconds) * 1000;
    let accepted = flag(window.accepted);
    (0_i64..)
        .zip(&window.samples)
        .map(|(i, value)| {
            let row = Row {
                ts_ms: start_ms + i * SAMPLE_MS,
                values: format!("{},{value},{accepted},{}", window.burst, window.motion),
            };
            (Stream::Ppg, row)
        })
        .collect()
}

/// Spacing of optical samples: 25 Hz.
const SAMPLE_MS: i64 = 40;

/// Gives each interval its own timestamp: the beat that ends it. The last interval ends at
/// the record's time and earlier ones are counted back from there. The strap does not send
/// per-beat times, so this is an approximation within one second.
fn rr_rows(record_ms: i64, intervals: &[u16]) -> impl Iterator<Item = Row> + '_ {
    let mut beat_ms = record_ms;
    let mut rows: Vec<Row> = intervals
        .iter()
        .rev()
        .map(|&ms| {
            let row = Row {
                ts_ms: beat_ms,
                values: ms.to_string(),
            };
            beat_ms -= i64::from(ms);
            row
        })
        .collect();
    rows.reverse();
    rows.into_iter()
}

/// Formats Unix seconds as a UTC calendar date, `YYYY-MM-DD`.
///
/// Uses the days-to-civil algorithm from <https://howardhinnant.github.io/date_algorithms.html>.
#[must_use]
pub fn utc_date(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(unix_seconds: u32) -> BiometricSummary {
        BiometricSummary {
            record_index: 1,
            unix_seconds,
            heart_rate: 61,
            rr_intervals_ms: vec![1000, 950],
            gravity: [0.0, -0.5, 0.866],
            skin_temperature_c: Some(34.12),
            motion: 0.25,
            motion_long: -5.0,
            step_counter: 1234,
            temperature_aux: [318, 329],
        }
    }

    #[test]
    fn formats_dates_in_utc() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(1_793_663_999), "2026-11-02");
        assert_eq!(utc_date(1_793_664_000), "2026-11-03");
    }

    #[test]
    fn makes_one_row_per_stream_and_one_per_interval() {
        let rows = rows(&summary(1_793_600_000));
        let csv = |stream: Stream| -> Vec<Row> {
            rows.iter()
                .filter(|(s, _)| *s == stream)
                .map(|(_, r)| r.clone())
                .collect()
        };
        let ts: i64 = 1_793_600_000_000;
        assert_eq!(
            to_csv(Stream::HeartRate, &csv(Stream::HeartRate)),
            format!("ts,bpm\n{ts},61\n")
        );
        assert_eq!(
            to_csv(Stream::RrInterval, &csv(Stream::RrInterval)),
            format!("ts,ms\n{},1000\n{ts},950\n", ts - 950)
        );
        assert_eq!(
            to_csv(Stream::Accel, &csv(Stream::Accel)),
            format!("ts,x,y,z\n{ts},0.0000,-0.5000,0.8660\n")
        );
        assert_eq!(
            to_csv(Stream::Temperature, &csv(Stream::Temperature)),
            format!("ts,celsius\n{ts},34.12\n")
        );
        assert_eq!(
            to_csv(Stream::Motion, &csv(Stream::Motion)),
            format!("ts,intensity,intensity_long\n{ts},0.25,-5\n")
        );
        assert_eq!(
            to_csv(Stream::Steps, &csv(Stream::Steps)),
            format!("ts,count\n{ts},1234\n")
        );
    }

    #[test]
    fn battery_rows_carry_the_last_known_percent_and_charging_state() {
        let mut battery = BatteryState::default();
        let first = battery.fact_row(1000, &Fact::Battery(30.8)).1;
        let second = battery.fact_row(2000, &Fact::Charging(true)).1;
        assert_eq!(first.values, "30.8,");
        assert_eq!(second.values, "30.8,1");
    }

    #[test]
    fn skips_zero_heart_rate_and_missing_temperature() {
        let mut off_wrist = summary(1_793_600_000);
        off_wrist.heart_rate = 0;
        off_wrist.skin_temperature_c = None;
        let streams: Vec<Stream> = rows(&off_wrist).into_iter().map(|(s, _)| s).collect();
        assert!(!streams.contains(&Stream::HeartRate));
        assert!(!streams.contains(&Stream::Temperature));
    }
}
