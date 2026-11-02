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
HEALTHSTRAP_TOKEN=... cargo run --release -- daemon --server https://your-server
```

`daemon` repeats sync then upload every 15 minutes (`--every-minutes`). A strap out of range or
a server that is down is logged and retried next time, so it can run unattended.

## Run on the Linux server

[`deploy/healthstrap.service`](deploy/healthstrap.service) is a systemd unit for `daemon`; the
install steps are at the top of the file. The token lives in `/etc/healthstrap/env`, readable
only by root, and the store in `/var/lib/healthstrap`. Pair the strap once with `bluetoothctl`
before starting it. If the service cannot reach the adapter, check that your distribution's
BlueZ policy allows the `bluetooth` group (`/etc/dbus-1/system.d/bluetooth.conf`).

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

**Linux** needs BlueZ running and the D-Bus headers to build:

- Arch: `sudo pacman -S --needed bluez bluez-utils rustup pkgconf` then `sudo systemctl enable --now bluetooth`
- Debian/Ubuntu: `sudo apt install bluez libdbus-1-dev pkg-config`

If `scan` finds nothing: turn off Bluetooth on your phone (a strap connected to it stops
advertising), check `bluetoothctl show` says `Powered: yes` (`rfkill unblock bluetooth` and
`bluetoothctl power on` if not), and try `healthstrap scan --all` to list every device in range.
### Pairing (once per computer)

The strap only answers commands over a paired, encrypted link, and it holds **one pairing at a
time**. If `sync` times out waiting for a reply, it is not paired with this computer:

1. Turn off Bluetooth on your phone, or forget the strap there.
2. `bluetoothctl remove <address>` to clear any half-finished pairing.
3. Put the strap in pairing mode: tap the sensor firmly and repeatedly until the LEDs flash blue.
4. In `bluetoothctl`: `scan on`, wait for the strap, then `pair <address>` and `trust <address>`.

`pair` failing with AuthenticationFailed means the pairing window closed; repeat steps 2–4.
`healthstrap sync --debug` prints every byte sent and received if it still stalls.

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
