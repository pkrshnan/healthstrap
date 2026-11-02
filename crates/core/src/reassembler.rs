//! Joins Bluetooth notifications back into whole frames.
//!
//! A frame can be split across notifications, and one notification can end one frame
//! and start the next. Bytes before a start marker are dropped.

use crate::frame::{Frame, FrameError, total_length};

/// Largest frame accepted. History records are at most 2,140 bytes; this leaves room.
const MAX_FRAME_LEN: usize = 4096;

/// Something that came out of the byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// A frame that passed every check.
    Frame(Frame),
    /// Bytes that looked like a frame but failed a check. Kept for diagnostics.
    Rejected {
        /// The bytes as received.
        bytes: Vec<u8>,
        /// The check they failed.
        error: FrameError,
    },
}

/// Buffers notification bytes for one connection.
#[derive(Debug, Default)]
pub struct Reassembler {
    buffer: Vec<u8>,
}

impl Reassembler {
    /// A reassembler with an empty buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Adds one notification and returns every frame it completed.
    pub fn push(&mut self, notification: &[u8]) -> Vec<Output> {
        self.buffer.extend_from_slice(notification);
        let mut outputs = Vec::new();
        loop {
            self.skip_to_start();
            let Some(length) = total_length(&self.buffer) else {
                break;
            };
            if length > MAX_FRAME_LEN {
                // Not a real frame start. Drop the marker and look for the next one.
                self.buffer.drain(..1);
                continue;
            }
            if self.buffer.len() < length {
                break;
            }
            let bytes: Vec<u8> = self.buffer.drain(..length).collect();
            outputs.push(match Frame::parse(bytes.clone()) {
                Ok(frame) => Output::Frame(frame),
                Err(error) => Output::Rejected { bytes, error },
            });
        }
        outputs
    }

    /// Drops any partial frame, for example after a disconnect.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    fn skip_to_start(&mut self) {
        let start = self
            .buffer
            .iter()
            .position(|&b| b == 0xAA)
            .unwrap_or(self.buffer.len());
        self.buffer.drain(..start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::encode_command;

    fn frames(outputs: &[Output]) -> usize {
        outputs
            .iter()
            .filter(|o| matches!(o, Output::Frame(_)))
            .count()
    }

    #[test]
    fn joins_a_frame_split_across_notifications() {
        let frame = encode_command(1, 26, &[1]);
        let (first, second) = frame.split_at(5);
        let mut reassembler = Reassembler::new();
        assert!(reassembler.push(first).is_empty());
        assert_eq!(frames(&reassembler.push(second)), 1);
    }

    #[test]
    fn splits_two_frames_in_one_notification_and_skips_noise() {
        let mut bytes = vec![0x00, 0x13];
        bytes.extend(encode_command(1, 26, &[1]));
        bytes.extend(encode_command(2, 34, &[]));
        assert_eq!(frames(&Reassembler::new().push(&bytes)), 2);
    }
}
