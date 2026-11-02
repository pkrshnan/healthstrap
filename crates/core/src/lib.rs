//! Protocol core for reading a WHOOP 5.0 strap.
//!
//! This crate knows how bytes look on the wire and how to turn them into samples.
//! It never touches Bluetooth, so every part of it can be tested with plain byte arrays.
//!
//! The layouts come from the protocol facts documented by the NOOP project
//! (<https://github.com/ryanbr/noop>). No NOOP code is used.

pub mod command;
pub mod crc;
pub mod frame;
pub mod gatt;
pub mod offload;
pub mod reassembler;
pub mod record;

mod bytes;
#[cfg(test)]
mod test_support;
