# healthstrap

Reads a WHOOP 5.0 strap over Bluetooth, with no phone app or cloud account, and exports
its data as one gzipped CSV per stream per day to the command-center ingest API.

Runs on Linux and macOS as a command-line daemon, and on iOS as a small app. All three share
one Rust core.

## Layout

| Path | What it does |
| --- | --- |
| `crates/core` | Frames, checksums, reassembly and record decoding. No Bluetooth, fully unit-tested. |

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
