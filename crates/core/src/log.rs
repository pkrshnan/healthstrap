//! The strap's firmware debug log, and the facts worth keeping from it.
//!
//! Debug-log packets carry plain text, NUL-padded, from byte 21. Joined in order they form
//! lines such as `12, 613874959: SUPERVISOR: SOC report: 30.80`: a boot count, milliseconds
//! since boot, then the message. The firmware's own decisions about wear, sleep, battery and
//! charging appear here and nowhere else that has been decoded yet.
//!
//! Every message matched below was seen on firmware 3.30.5.0. Other firmware may word them
//! differently, in which case they are simply not matched.

use crate::bytes::u32_at;
use crate::frame::{Frame, packet_type};

const TEXT_START: usize = 21;
const TRAILER_LEN: usize = 4;

/// One complete log line, stamped with the strap time of the packet it started in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Strap clock, Unix seconds.
    pub unix_seconds: u32,
    /// The message, without the boot count and uptime prefix.
    pub message: String,
}

/// Joins debug-log packets into lines. Feed packets in the order they were received.
#[derive(Debug, Default)]
pub struct Joiner {
    partial: String,
    started: Option<u32>,
}

impl Joiner {
    /// Creates a joiner with nothing buffered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one frame and returns the lines it completes. Other packet types are ignored.
    pub fn push(&mut self, frame: &Frame) -> Vec<Line> {
        let bytes = frame.bytes();
        let Some(unix_seconds) = u32_at(bytes, 12) else {
            return Vec::new();
        };
        if frame.packet_type() != packet_type::DEBUG_LOG {
            return Vec::new();
        }
        let end = bytes.len().saturating_sub(TRAILER_LEN);
        let text = bytes.get(TEXT_START..end).unwrap_or_default();
        let mut lines = Vec::new();
        for &byte in text.iter().filter(|&&byte| byte != 0) {
            if byte == b'\n' {
                if let Some(line) = self.finish() {
                    lines.push(line);
                }
            } else {
                self.started.get_or_insert(unix_seconds);
                self.partial.push(char::from(byte));
            }
        }
        lines
    }

    fn finish(&mut self) -> Option<Line> {
        let unix_seconds = self.started.take()?;
        let raw = std::mem::take(&mut self.partial);
        Some(Line {
            unix_seconds,
            message: strip_prefix(&raw).to_owned(),
        })
    }
}

/// Drops the `<boot>, <uptime ms>: ` prefix, if the line has one.
fn strip_prefix(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some((head, rest)) = trimmed.split_once(':') else {
        return trimmed;
    };
    let is_prefix = head.contains(',')
        && head
            .chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == ' ');
    if is_prefix { rest.trim() } else { trimmed }
}

/// A firmware decision or measurement found in one log line.
#[derive(Debug, Clone, PartialEq)]
pub enum Fact {
    /// The strap decided it went on (`true`) or came off (`false`) the body.
    Wear(bool),
    /// The strap's sleep state machine moved to this state, e.g. `SLEEP` or `WAKE`.
    SleepState(String),
    /// Battery state of charge, in percent.
    Battery(f32),
    /// Charging started (`true`) or stopped (`false`).
    Charging(bool),
    /// A double tap on the strap.
    DoubleTap,
    /// The strap computed a valid blood-oxygen reading during sleep. Its value is not sent.
    SpO2Computed,
}

impl Fact {
    /// Reads the fact in a log message, if it holds one this module knows.
    #[must_use]
    pub fn parse(message: &str) -> Option<Self> {
        match message {
            "SIGPROC: wear-detection moving from off-body to on-body" => Some(Self::Wear(true)),
            "SIGPROC: wear-detection moving from on-body to off-body" => Some(Self::Wear(false)),
            "LISTENER: Charging On" => Some(Self::Charging(true)),
            "LISTENER: Charging Off" => Some(Self::Charging(false)),
            "SENSORS: IMU double tap detected" => Some(Self::DoubleTap),
            "SIGPROC: generated a valid SPO2 during sleep" => Some(Self::SpO2Computed),
            _ => Self::parse_with_value(message),
        }
    }

    fn parse_with_value(message: &str) -> Option<Self> {
        if let Some(percent) = message.strip_prefix("SUPERVISOR: SOC report:") {
            return percent.trim().parse().ok().map(Self::Battery);
        }
        let transition = message.strip_prefix("SLEEPFLAG: moving from state ")?;
        let (_, to) = transition.split_once(" to '")?;
        let (state, _) = to.split_once('\'')?;
        Some(Self::SleepState(state.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::strap_frame;

    fn log_frame(unix_seconds: u32, text: &str) -> Frame {
        let mut body = vec![packet_type::DEBUG_LOG, 1, 2, 0];
        body.extend_from_slice(&unix_seconds.to_le_bytes());
        body.extend_from_slice(&[0, 0, 0, 0, 1]);
        body.extend_from_slice(text.as_bytes());
        body.push(0);
        strap_frame(&body)
    }

    #[test]
    fn joins_lines_across_packets_and_stamps_them_where_they_start() {
        let mut joiner = Joiner::new();
        let first = joiner.push(&log_frame(100, "\n 12, 613874959: SUPERVISOR: SOC re"));
        assert!(first.is_empty());
        let second = joiner.push(&log_frame(105, "port: 30.80\n 12, 61"));
        assert_eq!(
            second,
            vec![Line {
                unix_seconds: 100,
                message: "SUPERVISOR: SOC report: 30.80".to_owned(),
            }]
        );
    }

    #[test]
    fn reads_the_facts_worth_keeping() {
        let cases = [
            (
                "SIGPROC: wear-detection moving from on-body to off-body",
                Some(Fact::Wear(false)),
            ),
            ("SUPERVISOR: SOC report: 30.80", Some(Fact::Battery(30.8))),
            (
                "SLEEPFLAG: moving from state 'UP' to 'SLEEP', cnt = 102, motion_long = 0.01",
                Some(Fact::SleepState("SLEEP".to_owned())),
            ),
            ("LISTENER: Charging On", Some(Fact::Charging(true))),
            ("SENSORS: AFE configuration changed", None),
        ];
        for (message, fact) in cases {
            assert_eq!(Fact::parse(message), fact, "{message}");
        }
    }
}
