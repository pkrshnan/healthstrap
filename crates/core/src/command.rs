//! Commands the exporter sends, and the replies and history markers it reads back.

use crate::bytes::u8_at;
use crate::frame::{Frame, encode_command, packet_type};

/// Command numbers used by the exporter.
pub mod number {
    /// Battery percent.
    pub const GET_BATTERY_LEVEL: u8 = 26;
    /// Start sending stored history.
    pub const SEND_HISTORICAL_DATA: u8 = 22;
    /// Acknowledge a history chunk so the strap can free it.
    pub const HISTORICAL_DATA_RESULT: u8 = 23;
    /// Stored history range. Requested before history.
    pub const GET_DATA_RANGE: u8 = 34;
    /// Identity, battery and clock. The client hello.
    pub const GET_HELLO: u8 = 145;
    /// Set the strap's clock.
    pub const SET_CLOCK: u8 = 146;
    /// Read the strap's clock.
    pub const GET_CLOCK: u8 = 147;
}

/// Builds `SET_CLOCK`: revision 1, Unix seconds, then 1/32768-second ticks.
#[must_use]
pub fn set_clock(sequence: u8, unix_seconds: u32, ticks: u16) -> Vec<u8> {
    let mut payload = vec![1];
    payload.extend_from_slice(&unix_seconds.to_le_bytes());
    payload.extend_from_slice(&(ticks & 0x7FFF).to_le_bytes());
    encode_command(sequence, number::SET_CLOCK, &payload)
}

/// Builds `SEND_HISTORICAL_DATA` with its explicit `00` byte.
#[must_use]
pub fn send_historical_data(sequence: u8) -> Vec<u8> {
    encode_command(sequence, number::SEND_HISTORICAL_DATA, &[0])
}

/// Builds the acknowledgement for a chunk. Send only after the chunk is saved.
#[must_use]
pub fn acknowledge_chunk(sequence: u8, token: &EndToken) -> Vec<u8> {
    let mut payload = vec![1];
    payload.extend_from_slice(&token.0);
    encode_command(sequence, number::HISTORICAL_DATA_RESULT, &payload)
}

/// The outcome a reply reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The strap refused the request.
    Failure,
    /// Accepted. Not proof that asynchronous work finished.
    Success,
    /// Accepted; a final reply follows.
    Pending,
    /// The strap does not support this command here.
    Unsupported,
    /// A value with no known meaning, kept as is.
    Unknown(u8),
}

impl From<u8> for Outcome {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Failure,
            1 => Self::Success,
            2 => Self::Pending,
            3 => Self::Unsupported,
            other => Self::Unknown(other),
        }
    }
}

/// A reply to a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response<'a> {
    /// The command this answers.
    pub command: u8,
    /// The sequence number of the request it answers. Match on this, not byte 9.
    pub request_sequence: u8,
    /// What happened.
    pub outcome: Outcome,
    /// Command-specific bytes, padding included. Each command knows its own length.
    pub body: &'a [u8],
}

impl<'a> Response<'a> {
    /// Reads a reply, or `None` if the frame is something else.
    #[must_use]
    pub fn parse(frame: &'a Frame) -> Option<Self> {
        let bytes = frame.bytes();
        if frame.packet_type() != packet_type::COMMAND_RESPONSE {
            return None;
        }
        Some(Self {
            command: u8_at(bytes, 10)?,
            request_sequence: u8_at(bytes, 11)?,
            outcome: Outcome::from(u8_at(bytes, 12)?),
            body: bytes.get(13..bytes.len().checked_sub(4)?)?,
        })
    }
}

/// The eight opaque bytes that close a history chunk. Echo them back unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndToken(pub [u8; 8]);

impl EndToken {
    /// The strap's trim cursor, the first word of the token.
    #[must_use]
    pub const fn trim_cursor(&self) -> u32 {
        u32::from_le_bytes([self.0[0], self.0[1], self.0[2], self.0[3]])
    }
}

/// A history marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// History is about to stream.
    Start,
    /// A chunk ended. Save it, then acknowledge with this token.
    End(EndToken),
    /// This attempt is over.
    Complete,
    /// A marker type with no known meaning.
    Other(u8),
}

impl Marker {
    /// Reads a history marker, or `None` if the frame is something else.
    #[must_use]
    pub fn parse(frame: &Frame) -> Option<Self> {
        if frame.packet_type() != packet_type::METADATA {
            return None;
        }
        let bytes = frame.bytes();
        Some(match u8_at(bytes, 10)? {
            1 => Self::Start,
            2 => Self::End(EndToken(bytes.get(21..29)?.try_into().ok()?)),
            3 => Self::Complete,
            other => Self::Other(other),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::{crc16_modbus, crc32};

    /// Builds a frame as the strap would, with any packet type.
    fn strap_frame(body: &[u8]) -> Frame {
        let mut body = body.to_vec();
        body.resize(body.len().next_multiple_of(4), 0);
        let declared = u16::try_from(body.len() + 4).unwrap_or_default();
        let mut bytes = vec![0xAA, 0x01];
        bytes.extend_from_slice(&declared.to_le_bytes());
        bytes.extend_from_slice(&[0x01, 0x00]);
        bytes.extend_from_slice(&crc16_modbus(&bytes).to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(&crc32(&body).to_le_bytes());
        Frame::parse(bytes).unwrap_or_else(|e| unreachable!("test frame invalid: {e:?}"))
    }

    #[test]
    fn reads_a_battery_reply() {
        let frame = strap_frame(&[36, 9, number::GET_BATTERY_LEVEL, 4, 1, 47, 0, 0, 0]);
        let response = Response::parse(&frame);
        assert_eq!(response.as_ref().map(|r| r.request_sequence), Some(4));
        assert_eq!(response.as_ref().map(|r| r.outcome), Some(Outcome::Success));
        assert_eq!(response.and_then(|r| r.body.first().copied()), Some(47));
    }

    #[test]
    fn reads_an_end_marker_and_echoes_its_token() {
        let mut body = vec![49, 0, 2];
        body.resize(13, 0);
        body.extend_from_slice(&[0x10, 0, 0, 0, 0xAB, 0xCD, 0xEF, 0x01]);
        let frame = strap_frame(&body);

        let Some(Marker::End(token)) = Marker::parse(&frame) else {
            unreachable!("expected an end marker");
        };
        assert_eq!(token.trim_cursor(), 0x10);

        let ack = acknowledge_chunk(5, &token);
        assert_eq!(
            ack.get(11..20),
            Some(&[1, 0x10, 0, 0, 0, 0xAB, 0xCD, 0xEF, 0x01][..])
        );
    }
}
