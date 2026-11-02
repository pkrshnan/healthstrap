//! Format-1 frames, the envelope every WHOOP 5.0 message travels in.
//!
//! ```text
//! offset  0    1    2..4        4..6      6..8        8 ..             end-4 .. end
//!         AA   01   length u16  header    CRC16(0..6) body (padded)    CRC32(body)
//! ```
//!
//! `length` is the padded body size plus four. The body starts with
//! packet type, sequence and command (or record layout), then its payload,
//! zero-padded to a multiple of four bytes.

use crate::bytes::{u8_at, u16_at, u32_at};
use crate::crc::{crc16_modbus, crc32};

const START: u8 = 0xAA;
const FORMAT_1: u8 = 0x01;
const HEADER_LEN: usize = 8;
const TRAILER_LEN: usize = 4;
const BODY_START: usize = HEADER_LEN;

/// What a frame carries, read from byte 8.
pub mod packet_type {
    /// A command sent to the strap.
    pub const COMMAND: u8 = 35;
    /// The strap's reply to a command.
    pub const COMMAND_RESPONSE: u8 = 36;
    /// Live heart rate and R-R intervals.
    pub const LIVE_HEART_RATE: u8 = 40;
    /// A stored history record. Byte 9 says which layout.
    pub const HISTORICAL: u8 = 47;
    /// An event, such as a tap or a charger change.
    pub const EVENT: u8 = 48;
    /// History start, chunk end and complete markers.
    pub const METADATA: u8 = 49;
}

/// Why bytes were not accepted as a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer bytes than the smallest possible frame.
    TooShort,
    /// The first two bytes are not `AA 01`.
    NotFormat1,
    /// The declared length disagrees with the number of bytes.
    LengthMismatch,
    /// The header checksum failed.
    HeaderCrc,
    /// The body checksum failed.
    BodyCrc,
}

/// A frame whose length and both checksums have been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    bytes: Vec<u8>,
}

impl Frame {
    /// Checks `bytes` and wraps them if they form exactly one valid frame.
    ///
    /// # Errors
    ///
    /// Returns the first check that failed.
    pub fn parse(bytes: Vec<u8>) -> Result<Self, FrameError> {
        if bytes.len() <= 15 {
            return Err(FrameError::TooShort);
        }
        if bytes.first() != Some(&START) || bytes.get(1) != Some(&FORMAT_1) {
            return Err(FrameError::NotFormat1);
        }
        let declared = declared_length(&bytes).ok_or(FrameError::TooShort)?;
        if declared + HEADER_LEN != bytes.len() || (declared - TRAILER_LEN) % 4 != 0 {
            return Err(FrameError::LengthMismatch);
        }
        let header_crc = u16_at(&bytes, 6).ok_or(FrameError::TooShort)?;
        if crc16_modbus(bytes.get(..6).unwrap_or_default()) != header_crc {
            return Err(FrameError::HeaderCrc);
        }
        let body_end = bytes.len() - TRAILER_LEN;
        let body_crc = u32_at(&bytes, body_end).ok_or(FrameError::TooShort)?;
        if crc32(bytes.get(BODY_START..body_end).unwrap_or_default()) != body_crc {
            return Err(FrameError::BodyCrc);
        }
        Ok(Self { bytes })
    }

    /// The whole frame, header and trailer included. Offsets in this crate index into this.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The packet type at byte 8. See [`packet_type`].
    #[must_use]
    pub fn packet_type(&self) -> u8 {
        u8_at(&self.bytes, BODY_START).unwrap_or_default()
    }
}

/// Builds a command frame with the given request sequence number.
#[must_use]
pub fn encode_command(sequence: u8, command: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![packet_type::COMMAND, sequence, command];
    body.extend_from_slice(payload);
    body.resize(body.len().next_multiple_of(4), 0);

    let declared = u16::try_from(body.len() + TRAILER_LEN).unwrap_or(u16::MAX);
    let mut frame = vec![START, FORMAT_1];
    frame.extend_from_slice(&declared.to_le_bytes());
    frame.extend_from_slice(&[0x00, 0x01]);
    frame.extend_from_slice(&crc16_modbus(&frame).to_le_bytes());
    frame.extend_from_slice(&body);
    frame.extend_from_slice(&crc32(&body).to_le_bytes());
    frame
}

/// Total size of the frame that starts at the beginning of `bytes`, once enough is known.
pub(crate) fn total_length(bytes: &[u8]) -> Option<usize> {
    declared_length(bytes).map(|declared| declared + HEADER_LEN)
}

fn declared_length(bytes: &[u8]) -> Option<usize> {
    u16_at(bytes, 2).map(usize::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed hello every session starts with: `GET_HELLO` (145), revision 1, sequence 1.
    const CLIENT_HELLO: [u8; 16] = [
        0xAA, 0x01, 0x08, 0x00, 0x00, 0x01, 0xE6, 0x71, 0x23, 0x01, 0x91, 0x01, 0x36, 0x3E, 0x5C,
        0x8D,
    ];

    #[test]
    fn encodes_the_documented_client_hello() {
        assert_eq!(encode_command(1, 145, &[1]), CLIENT_HELLO);
    }

    #[test]
    fn accepts_a_valid_frame() {
        let frame = Frame::parse(CLIENT_HELLO.to_vec());
        assert_eq!(frame.map(|f| f.packet_type()), Ok(packet_type::COMMAND));
    }

    #[test]
    fn pads_the_body_to_four_bytes() {
        let frame = encode_command(7, 22, &[0]);
        assert_eq!(frame.len(), 16);
        assert!(Frame::parse(frame).is_ok());
    }

    #[test]
    fn rejects_a_corrupted_body() {
        let mut bytes = CLIENT_HELLO.to_vec();
        bytes[11] ^= 0xFF;
        assert_eq!(Frame::parse(bytes), Err(FrameError::BodyCrc));
    }

    #[test]
    fn rejects_a_corrupted_header() {
        let mut bytes = CLIENT_HELLO.to_vec();
        bytes[5] = 0x00;
        assert_eq!(Frame::parse(bytes), Err(FrameError::HeaderCrc));
    }

    #[test]
    fn rejects_a_truncated_frame() {
        let mut bytes = CLIENT_HELLO.to_vec();
        bytes.push(0);
        assert_eq!(Frame::parse(bytes), Err(FrameError::LengthMismatch));
    }
}
