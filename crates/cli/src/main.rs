//! `healthstrap`: sync a WHOOP 5.0 strap over Bluetooth on macOS and Linux.

mod ble;
mod store;
mod sync;
mod upload;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List nearby straps.
    Scan {
        /// Seconds to scan for.
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// List every Bluetooth device seen, not just straps. For troubleshooting.
        #[arg(long)]
        all: bool,
    },
    /// Download the strap's stored history into the local store.
    Sync {
        /// Strap to use, as `scan` prints it. Defaults to the only strap found.
        #[arg(long)]
        device: Option<String>,
        /// Path of the local store.
        #[arg(long, default_value = "healthstrap.db")]
        db: PathBuf,
        /// Seconds to scan for the strap.
        #[arg(long, default_value_t = 15)]
        seconds: u64,
        /// Print every byte sent to and received from the strap, as hex.
        #[arg(long)]
        debug: bool,
    },
    /// Send rows not yet uploaded to the command-center server.
    /// Reads the API token from the `HEALTHSTRAP_TOKEN` environment variable.
    Upload {
        /// Server base URL, for example `https://home.tailnet.ts.net`.
        #[arg(long)]
        server: String,
        /// Path of the local store.
        #[arg(long, default_value = "healthstrap.db")]
        db: PathBuf,
    },
    /// Keep running: whenever the strap comes into range, sync and upload, then sync again every
    /// `--every-minutes` while it stays. For a computer that is always on near the strap.
    /// Reads the API token from the `HEALTHSTRAP_TOKEN` environment variable.
    Daemon {
        /// Server base URL, for example `https://home.tailnet.ts.net`.
        #[arg(long)]
        server: String,
        /// Strap to use, as `scan` prints it. Defaults to the only strap found.
        #[arg(long)]
        device: Option<String>,
        /// Path of the local store.
        #[arg(long, default_value = "healthstrap.db")]
        db: PathBuf,
        /// Minutes to wait between syncs while the strap is in range.
        #[arg(long, default_value_t = 5)]
        every_minutes: u64,
    },
    /// Write every stored record as CSV files, one per stream per day.
    Export {
        /// Folder to write into.
        #[arg(long)]
        out: PathBuf,
        /// Path of the local store.
        #[arg(long, default_value = "healthstrap.db")]
        db: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Scan { seconds, all } => scan(seconds, all).await?,
        Command::Sync {
            device,
            db,
            seconds,
            debug,
        } => sync(device.as_deref(), &db, seconds, debug).await?,
        Command::Upload { server, db } => upload(&db, &server, &token()?)?,
        Command::Daemon {
            server,
            device,
            db,
            every_minutes,
        } => daemon(&server, device.as_deref(), &db, every_minutes).await?,
        Command::Export { out, db } => {
            let files = upload::export_all(&store::Store::open(&db)?, &out)?;
            println!("wrote {files} files to {}", out.display());
        }
    }
    Ok(())
}

async fn scan(seconds: u64, all: bool) -> Result<()> {
    let found = ble::scan(Duration::from_secs(seconds), all).await?;
    if found.is_empty() {
        eprintln!("{NOT_FOUND}");
    }
    for device in found {
        let connected = if device.connected { "\tconnected" } else { "" };
        let name = device.name.unwrap_or_else(|| "(no name)".to_owned());
        println!("{}\t{name}{connected}", device.id);
    }
    Ok(())
}

async fn sync(device: Option<&str>, db: &Path, seconds: u64, debug: bool) -> Result<()> {
    let strap = pick(
        ble::scan(Duration::from_secs(seconds), false).await?,
        device,
    )?;
    sync_strap(strap, db, debug).await
}

async fn sync_strap(strap: ble::Found, db: &Path, debug: bool) -> Result<()> {
    let mut store = store::Store::open(db)?;
    println!("syncing {}", strap.id);
    let link = ble::Link::open(strap.peripheral, debug).await?;
    let result = sync::run(&link, &mut store, &strap.id).await;
    link.close().await;
    let summary = result?;
    let status = if summary.complete {
        "history complete"
    } else {
        "stopped early; run again"
    };
    println!(
        "{} chunks acknowledged, {} new frames, {status}",
        summary.chunks, summary.new_frames
    );
    Ok(())
}

fn upload(db: &Path, server: &str, token: &str) -> Result<()> {
    let summary = upload::upload(&store::Store::open(db)?, server, token)?;
    println!("uploaded {} rows in {} files", summary.rows, summary.files);
    Ok(())
}

/// How long one listen for the strap lasts before the daemon checks on other work, such as an
/// upload that failed earlier.
const LISTEN: Duration = Duration::from_secs(5 * 60);

/// How long to wait before trying again when the Bluetooth adapter itself is unavailable.
const ADAPTER_RETRY: Duration = Duration::from_secs(60);

/// Listens for the strap and syncs whenever it is in range. Nothing that goes wrong with the
/// strap, the adapter or the server stops the loop: each is logged and tried again later.
async fn daemon(server: &str, device: Option<&str>, db: &Path, every_minutes: u64) -> Result<()> {
    let token = token()?;
    let every = Duration::from_secs(every_minutes.saturating_mul(60));
    let mut state = Daemon::default();
    println!("listening for the strap");
    loop {
        match ble::wait_for_strap(device, LISTEN).await {
            Ok(Some(strap)) => {
                state.arrived();
                let synced = sync_strap(strap, db, false).await;
                state.synced(synced);
                state.uploaded(upload_blocking(db, server, &token));
                tokio::time::sleep(state.pause(every)).await;
            }
            Ok(None) => {
                state.away();
                if state.upload_pending {
                    state.uploaded(upload_blocking(db, server, &token));
                }
            }
            Err(error) => {
                eprintln!("Bluetooth unavailable: {error:#}");
                tokio::time::sleep(ADAPTER_RETRY).await;
            }
        }
    }
}

/// Uploading blocks on the network; let the runtime move other work off this thread.
fn upload_blocking(db: &Path, server: &str, token: &str) -> Result<()> {
    tokio::task::block_in_place(|| upload(db, server, token))
}

/// What the daemon remembers between rounds, so it logs changes rather than every round and
/// backs off when syncing keeps failing.
#[derive(Default)]
struct Daemon {
    /// Whether the last listen heard nothing.
    away: bool,
    /// Syncs that failed in a row while the strap was in range.
    failures: u32,
    /// Whether the last upload failed, so rows are waiting to be sent.
    upload_pending: bool,
}

impl Daemon {
    fn arrived(&mut self) {
        if self.away {
            println!("strap in range");
        }
        self.away = false;
    }

    fn away(&mut self) {
        if !self.away {
            println!("strap out of range; listening");
        }
        self.away = true;
        self.failures = 0;
    }

    fn synced(&mut self, result: Result<()>) {
        match result {
            Ok(()) => self.failures = 0,
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                eprintln!("sync failed: {error:#}");
            }
        }
    }

    fn uploaded(&mut self, result: Result<()>) {
        self.upload_pending = result.is_err();
        if let Err(error) = result {
            eprintln!("upload failed: {error:#}");
        }
    }

    /// The wait before the next sync: `every` after a good sync. After failures it starts at a
    /// minute and doubles each time, up to `every`, so a strap that drops the link mid-sync is
    /// retried soon without being hammered.
    fn pause(&self, every: Duration) -> Duration {
        if self.failures == 0 {
            return every;
        }
        let doublings = self.failures.saturating_sub(1).min(16);
        Duration::from_secs(60u64 << doublings).min(every)
    }
}

fn token() -> Result<String> {
    std::env::var("HEALTHSTRAP_TOKEN").context("set HEALTHSTRAP_TOKEN to the server's API token")
}

const NOT_FOUND: &str = "no strap found. Turn off Bluetooth on your phone (a strap connected to \
     it stops advertising), keep the strap close and off the charger, then try again. \
     `healthstrap scan --all` lists every device this computer can see.";

fn pick(found: Vec<ble::Found>, wanted: Option<&str>) -> Result<ble::Found> {
    let mut matches: Vec<_> = found
        .into_iter()
        .filter(|f| wanted.is_none_or(|w| f.id == w || f.name.as_deref() == Some(w)))
        .collect();
    match matches.len() {
        0 => bail!(NOT_FOUND),
        1 => Ok(matches.remove(0)),
        _ => bail!("several straps found; pick one with --device (see `healthstrap scan`)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY: Duration = Duration::from_secs(15 * 60);

    #[test]
    fn waits_the_full_interval_after_a_good_sync() {
        assert_eq!(Daemon::default().pause(EVERY), EVERY);
    }

    #[test]
    fn backs_off_after_failures_up_to_the_interval() {
        let mut state = Daemon::default();
        let mut pauses = Vec::new();
        for _ in 0..6 {
            state.synced(Err(anyhow::anyhow!("link lost")));
            pauses.push(state.pause(EVERY).as_secs() / 60);
        }
        assert_eq!(pauses, [1, 2, 4, 8, 15, 15]);
        state.synced(Ok(()));
        assert_eq!(state.pause(EVERY), EVERY);
    }

    #[test]
    fn leaving_range_resets_the_backoff() {
        let mut state = Daemon::default();
        state.synced(Err(anyhow::anyhow!("link lost")));
        state.away();
        assert_eq!(state.pause(EVERY), EVERY);
    }

    #[test]
    fn remembers_a_failed_upload() {
        let mut state = Daemon::default();
        state.uploaded(Err(anyhow::anyhow!("server down")));
        assert!(state.upload_pending);
        state.uploaded(Ok(()));
        assert!(!state.upload_pending);
    }
}
