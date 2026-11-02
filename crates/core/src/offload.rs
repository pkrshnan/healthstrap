//! The history offload state machine, with no I/O.
//!
//! Feed it every frame that arrives after `SEND_HISTORICAL_DATA`. It groups records into
//! chunks and says when a chunk is complete. The caller saves the chunk durably, then sends
//! [`crate::command::acknowledge_chunk`] with the chunk's token. Acknowledging lets the
//! strap delete those records, so never acknowledge before the save has committed.

use crate::command::{EndToken, Marker};
use crate::frame::{Frame, packet_type};

/// What the caller should do after a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing yet; keep reading.
    Continue,
    /// A chunk closed. Save `frames`, then acknowledge with `token`.
    ChunkReady {
        /// Every record frame in the chunk, in arrival order.
        frames: Vec<Frame>,
        /// The chunk's token, to echo back unchanged.
        token: EndToken,
    },
    /// The strap finished this attempt. Records after the last chunk were not acknowledged
    /// and will be sent again next time; they are returned so they can be kept anyway.
    Complete {
        /// Records received after the last chunk boundary.
        unacknowledged: Vec<Frame>,
    },
}

/// Collects one offload attempt.
#[derive(Debug, Default)]
pub struct Offload {
    pending: Vec<Frame>,
}

impl Offload {
    /// A new attempt with nothing collected.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Takes one frame and says what to do next.
    pub fn on_frame(&mut self, frame: Frame) -> Step {
        match Marker::parse(&frame) {
            Some(Marker::End(token)) => Step::ChunkReady {
                frames: std::mem::take(&mut self.pending),
                token,
            },
            Some(Marker::Complete) => Step::Complete {
                unacknowledged: std::mem::take(&mut self.pending),
            },
            Some(Marker::Start | Marker::Other(_)) => Step::Continue,
            None => {
                if is_record(&frame) {
                    self.pending.push(frame);
                }
                Step::Continue
            }
        }
    }
}

/// Command replies and live heart rate are not history and are not saved with a chunk.
fn is_record(frame: &Frame) -> bool {
    !matches!(
        frame.packet_type(),
        packet_type::COMMAND_RESPONSE | packet_type::LIVE_HEART_RATE
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::strap_frame;

    fn marker(kind: u8, token: [u8; 8]) -> Frame {
        let mut body = vec![packet_type::METADATA, 0, kind];
        body.resize(13, 0);
        body.extend_from_slice(&token);
        strap_frame(&body)
    }

    fn record(index: u8) -> Frame {
        strap_frame(&[packet_type::HISTORICAL, 18, 0, index])
    }

    #[test]
    fn hands_over_each_chunk_with_its_token() {
        let mut offload = Offload::new();
        assert_eq!(offload.on_frame(marker(1, [0; 8])), Step::Continue);
        assert_eq!(offload.on_frame(record(1)), Step::Continue);
        assert_eq!(
            offload.on_frame(strap_frame(&[packet_type::LIVE_HEART_RATE])),
            Step::Continue
        );
        assert_eq!(offload.on_frame(record(2)), Step::Continue);
        assert_eq!(
            offload.on_frame(marker(2, [7; 8])),
            Step::ChunkReady {
                frames: vec![record(1), record(2)],
                token: EndToken([7; 8])
            }
        );
        assert_eq!(offload.on_frame(record(3)), Step::Continue);
        assert_eq!(
            offload.on_frame(marker(3, [0; 8])),
            Step::Complete {
                unacknowledged: vec![record(3)]
            }
        );
    }
}
