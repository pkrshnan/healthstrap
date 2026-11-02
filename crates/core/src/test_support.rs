//! Helpers shared by unit tests.

use crate::crc::{crc16_modbus, crc32};
use crate::frame::Frame;

/// Builds a valid frame the way the strap sends it, around any body.
pub fn strap_frame(body: &[u8]) -> Frame {
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
