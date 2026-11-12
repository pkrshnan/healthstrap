//! Measurements decoded from live and stored records.
//!
//! Offsets are absolute in the whole frame. Fields with no established meaning are left out
//! here; the raw frame is kept elsewhere so they can be decoded later.

use crate::bytes::{f32_at, i16_at, i32_at, u8_at, u16_at, u32_at};
use crate::frame::{Frame, packet_type};

/// Layout byte (frame byte 9) of the per-second biometric summary.
pub const LAYOUT_BIOMETRIC: u8 = 18;

const BIOMETRIC_LEN: usize = 124;
const MAX_RR_PER_SUMMARY: usize = 4;

/// One stored biometric summary, written by the strap about once per second.
#[derive(Debug, Clone, PartialEq)]
pub struct BiometricSummary {
    /// Strap's record counter. Not a timestamp; never use it to fill gaps.
    pub record_index: u32,
    /// Strap clock, Unix seconds.
    pub unix_seconds: u32,
    /// Heart rate in bpm.
    pub heart_rate: u8,
    /// R-R intervals in milliseconds, zeros dropped.
    pub rr_intervals_ms: Vec<u16>,
    /// Gravity vector in g.
    pub gravity: [f32; 3],
    /// Skin temperature in °C, if within 5–45 °C.
    pub skin_temperature_c: Option<f32>,
    /// Motion intensity, unscaled. Inferred: about 0.006 asleep and 0.03 awake.
    pub motion: f32,
    /// Looks like the log of motion over a longer window. Inferred, window unknown.
    pub motion_long: f32,
    /// Cumulative counter that only rises while awake, about 8,000 a day. Inferred to be
    /// steps. It is 16 bits, so it wraps, and it may reset when the strap reboots.
    pub step_counter: u16,
    /// Two more temperature-like readings, raw. Possibly °C × 10. Meaning not confirmed.
    pub temperature_aux: [u16; 2],
}

impl BiometricSummary {
    /// Decodes a summary, or `None` if the frame is some other record.
    #[must_use]
    pub fn parse(frame: &Frame) -> Option<Self> {
        let bytes = frame.bytes();
        if frame.packet_type() != packet_type::HISTORICAL
            || u8_at(bytes, 9)? != LAYOUT_BIOMETRIC
            || bytes.len() != BIOMETRIC_LEN
        {
            return None;
        }
        let rr_count = usize::from(u8_at(bytes, 23)?).min(MAX_RR_PER_SUMMARY);
        Some(Self {
            record_index: u32_at(bytes, 11)?,
            unix_seconds: u32_at(bytes, 15)?,
            heart_rate: u8_at(bytes, 22)?,
            rr_intervals_ms: rr_intervals(bytes, 24, rr_count),
            gravity: [f32_at(bytes, 45)?, f32_at(bytes, 49)?, f32_at(bytes, 53)?],
            skin_temperature_c: skin_temperature(u16_at(bytes, 73)?),
            motion: f32_at(bytes, 41)?,
            motion_long: f32_at(bytes, 113)?,
            step_counter: u16_at(bytes, 57)?,
            temperature_aux: [u16_at(bytes, 69)?, u16_at(bytes, 71)?],
        })
    }
}

/// Layout byte (frame byte 9) of the one-second raw optical window.
pub const LAYOUT_OPTICAL: u8 = 26;

const OPTICAL_LEN: usize = 88;
const OPTICAL_DELTAS: usize = 24;
const OPTICAL_DELTA_START: usize = 27;
/// A delta at either limit was clipped, so the true jump is lost.
const CLIPPED: [i16; 2] = [i16::MAX, -i16::MAX];

/// One second of raw optical (PPG) samples at 25 Hz, stored by the strap in short bursts.
///
/// Layout as documented by NOOP and checked against a real capture.
#[derive(Debug, Clone, PartialEq)]
pub struct OpticalWindow {
    /// Strap clock, Unix seconds, of the first sample.
    pub unix_seconds: u32,
    /// Burst counter: rises once per burst, about every 20 minutes, and sometimes resets.
    pub burst: u16,
    /// Raw sample values, oldest first, 1/25 s apart. Cut short at a clipped jump.
    pub samples: Vec<i64>,
    /// Whether the strap accepted the window's signal.
    pub accepted: bool,
    /// Motion during the window, unscaled.
    pub motion: f32,
}

impl OpticalWindow {
    /// Decodes a window, or `None` if the frame is some other record.
    #[must_use]
    pub fn parse(frame: &Frame) -> Option<Self> {
        let bytes = frame.bytes();
        if frame.packet_type() != packet_type::HISTORICAL
            || u8_at(bytes, 9)? != LAYOUT_OPTICAL
            || bytes.len() != OPTICAL_LEN
        {
            return None;
        }
        let mut value = i64::from(i32_at(bytes, 23)?);
        let mut samples = vec![value];
        for i in 0..OPTICAL_DELTAS {
            let delta = i16_at(bytes, OPTICAL_DELTA_START + 2 * i)?;
            if CLIPPED.contains(&delta) {
                break;
            }
            value += i64::from(delta);
            samples.push(value);
        }
        Some(Self {
            unix_seconds: u32_at(bytes, 15)?,
            burst: u16_at(bytes, 21)?,
            samples,
            accepted: u8_at(bytes, 82)? == 1,
            motion: f32_at(bytes, 75)?,
        })
    }
}

/// A live heart-rate packet, sent while connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveHeartRate {
    /// Strap clock, Unix seconds.
    pub unix_seconds: u32,
    /// Heart rate in bpm.
    pub heart_rate: u8,
    /// R-R intervals in milliseconds, zeros dropped.
    pub rr_intervals_ms: Vec<u16>,
}

impl LiveHeartRate {
    /// Decodes a live packet, or `None` if the frame is something else.
    #[must_use]
    pub fn parse(frame: &Frame) -> Option<Self> {
        let bytes = frame.bytes();
        if frame.packet_type() != packet_type::LIVE_HEART_RATE {
            return None;
        }
        let declared = usize::from(u8_at(bytes, 17)?);
        let room = bytes.len().saturating_sub(4 + 18) / 2;
        Some(Self {
            unix_seconds: u32_at(bytes, 10)?,
            heart_rate: u8_at(bytes, 16)?,
            rr_intervals_ms: rr_intervals(bytes, 18, declared.min(room)),
        })
    }
}

/// Reads `count` R-R words, each already in milliseconds.
///
/// Not the 1/1024 s of the Bluetooth Heart Rate Service: read as ms, the intervals add up to the
/// elapsed time and match the strap's own heart rate, and read as 1/1024 s they come out 2.34%
/// short.
fn rr_intervals(bytes: &[u8], start: usize, count: usize) -> Vec<u16> {
    (0..count)
        .filter_map(|i| u16_at(bytes, start + 2 * i))
        .filter(|&ms| ms > 0)
        .collect()
}

fn skin_temperature(raw: u16) -> Option<f32> {
    let celsius = f32::from(raw) / 100.0;
    (5.0..=45.0).contains(&celsius).then_some(celsius)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::strap_frame;

    /// A summary body (frame bytes 8..120) with fields set at their frame offsets.
    fn biometric_body() -> Vec<u8> {
        let mut frame = vec![0_u8; 120];
        let mut put = |offset: usize, value: &[u8]| {
            frame.splice(offset..offset + value.len(), value.iter().copied());
        };
        put(8, &[packet_type::HISTORICAL, LAYOUT_BIOMETRIC]);
        put(11, &42_u32.to_le_bytes());
        put(15, &1_793_600_000_u32.to_le_bytes());
        put(22, &[61, 2]);
        put(24, &1024_u16.to_le_bytes());
        put(26, &980_u16.to_le_bytes());
        put(41, &0.25_f32.to_le_bytes());
        put(57, &1234_u16.to_le_bytes());
        put(69, &318_u16.to_le_bytes());
        put(71, &329_u16.to_le_bytes());
        put(113, &(-5.0_f32).to_le_bytes());
        put(45, &0.0_f32.to_le_bytes());
        put(49, &0.0_f32.to_le_bytes());
        put(53, &1.0_f32.to_le_bytes());
        put(73, &3412_u16.to_le_bytes());
        frame.split_off(8)
    }

    #[test]
    fn decodes_a_biometric_summary() {
        let summary = BiometricSummary::parse(&strap_frame(&biometric_body()));
        assert_eq!(
            summary,
            Some(BiometricSummary {
                record_index: 42,
                unix_seconds: 1_793_600_000,
                heart_rate: 61,
                rr_intervals_ms: vec![1024, 980],
                gravity: [0.0, 0.0, 1.0],
                skin_temperature_c: Some(34.12),
                motion: 0.25,
                motion_long: -5.0,
                step_counter: 1234,
                temperature_aux: [318, 329],
            })
        );
    }

    #[test]
    fn decodes_an_optical_window_and_stops_at_a_clipped_jump() {
        let mut frame = vec![0_u8; 84];
        let mut put = |offset: usize, value: &[u8]| {
            frame.splice(offset..offset + value.len(), value.iter().copied());
        };
        put(8, &[packet_type::HISTORICAL, LAYOUT_OPTICAL]);
        put(15, &1_793_600_000_u32.to_le_bytes());
        put(21, &7_u16.to_le_bytes());
        put(23, &(-16_i32).to_le_bytes());
        put(27, &5_i16.to_le_bytes());
        put(29, &(-3_i16).to_le_bytes());
        put(31, &i16::MAX.to_le_bytes());
        put(75, &0.5_f32.to_le_bytes());
        put(82, &[1]);
        let window = OpticalWindow::parse(&strap_frame(&frame.split_off(8)));
        assert_eq!(
            window,
            Some(OpticalWindow {
                unix_seconds: 1_793_600_000,
                burst: 7,
                samples: vec![-16, -11, -14],
                accepted: true,
                motion: 0.5,
            })
        );
    }

    #[test]
    fn drops_an_implausible_skin_temperature() {
        let mut body = biometric_body();
        body.splice(65..67, 0_u16.to_le_bytes());
        let summary = BiometricSummary::parse(&strap_frame(&body));
        assert_eq!(summary.and_then(|s| s.skin_temperature_c), None);
    }

    #[test]
    fn decodes_live_heart_rate_and_skips_zero_intervals() {
        let mut body = vec![packet_type::LIVE_HEART_RATE, 0];
        body.extend_from_slice(&1_793_600_000_u32.to_le_bytes());
        body.extend_from_slice(&[0, 0, 72, 2]);
        body.extend_from_slice(&0_u16.to_le_bytes());
        body.extend_from_slice(&850_u16.to_le_bytes());
        let live = LiveHeartRate::parse(&strap_frame(&body));
        assert_eq!(
            live.map(|l| (l.heart_rate, l.rr_intervals_ms)),
            Some((72, vec![850]))
        );
    }
}
