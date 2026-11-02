# healthstrap

Reads a WHOOP 5.0 strap over Bluetooth, with no phone app or cloud account, and exports
its data as one gzipped CSV per stream per day to the command-center ingest API.

Runs on Linux and macOS as a command-line daemon, and on iOS as a small app. All three share
one Rust core.

## Layout

| Path | What it does |
| --- | --- |
| `crates/core` | Frames, checksums, reassembly, record decoding and the offload state machine. No Bluetooth, fully unit-tested. |
| `crates/cli` | The `healthstrap` command for macOS and Linux. |

## Use

Close or log out of the WHOOP app first: only one device can hold the strap's connection.

```sh
cargo run --release -- scan            # list nearby straps
cargo run --release -- sync            # download history into healthstrap.db
cargo run --release -- export --out csv  # write csv/<stream>/<day>.csv to look at
HEALTHSTRAP_TOKEN=... cargo run --release -- upload --server https://your-server
```

`upload` sends only rows it has not sent before, as one gzipped CSV per stream per UTC day:
`POST <server>/api/ingest/samples/<stream>?day=YYYY-MM-DD` with `Authorization: Bearer <token>`,
`Content-Type: text/csv` and `Content-Encoding: gzip`. The server merges rows by `ts`, so a
retried upload is harmless.

| Stream | Columns | Notes |
| --- | --- | --- |
| `heart_rate` | `ts,bpm` | About once a second; zero (off wrist) is skipped |
| `rr_interval` | `ts,ms` | `ts` is the beat that ends the interval, counted back from the record time |
| `accel` | `ts,x,y,z` | Gravity vector in g, about once a second |
| `temperature` | `ts,celsius` | Skin temperature; values outside 5–45 °C are skipped |

`sync` saves every chunk of history to `healthstrap.db` and only then confirms it to the strap,
which lets the strap delete it. Run it again any time; frames it already has are skipped.

**Linux** needs BlueZ and the D-Bus headers to build (`sudo apt install bluez libdbus-1-dev
pkg-config`). If writes fail with an authentication error, pair once with `bluetoothctl`:
`scan on`, then `pair <address>` and `trust <address>`.

**macOS** asks for Bluetooth permission for your terminal the first time.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Credits

The wire format comes from the protocol facts documented by
[NOOP](https://github.com/ryanbr/noop), which build on `johnmiddleton12/my-whoop` and
`b-nnett/goose`. No NOOP code is included. This project is not affiliated with WHOOP.
Use it only with a strap you own.
