//! Bluetooth identifiers for the WHOOP 5.0 / MG custom service.

/// The custom service. Scan for this to find the strap.
pub const SERVICE: &str = "fd4b0001-cce1-4033-93ce-002d5875f58a";

/// Commands are written here, with response.
pub const COMMAND_WRITE: &str = "fd4b0002-cce1-4033-93ce-002d5875f58a";

/// Every characteristic to subscribe to. Replies, events, history and data share them.
pub const NOTIFY: [&str; 4] = [
    "fd4b0003-cce1-4033-93ce-002d5875f58a",
    "fd4b0004-cce1-4033-93ce-002d5875f58a",
    "fd4b0005-cce1-4033-93ce-002d5875f58a",
    "fd4b0007-cce1-4033-93ce-002d5875f58a",
];
