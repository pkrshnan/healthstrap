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
    /// Keep running: sync and upload, then wait, forever. For a server near the strap.
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
        /// Minutes to wait between runs.
        #[arg(long, default_value_t = 15)]
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
        Command::Scan { seconds } => {
            for strap in ble::scan(Duration::from_secs(seconds)).await? {
                println!("{}\t{}", strap.id, strap.name.unwrap_or_default());
            }
        }
        Command::Sync {
            device,
            db,
            seconds,
        } => sync(device.as_deref(), &db, seconds).await?,
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

async fn sync(device: Option<&str>, db: &Path, seconds: u64) -> Result<()> {
    let mut store = store::Store::open(db)?;
    let strap = pick(ble::scan(Duration::from_secs(seconds)).await?, device)?;
    println!("syncing {}", strap.id);
    let link = ble::Link::open(strap.peripheral).await?;
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

/// Runs sync then upload on a schedule. A failed step is logged and retried next run, so a
/// strap out of range or a server that is down never stops the loop.
async fn daemon(server: &str, device: Option<&str>, db: &Path, every_minutes: u64) -> Result<()> {
    let token = token()?;
    let pause = Duration::from_secs(every_minutes.saturating_mul(60));
    loop {
        if let Err(error) = sync(device, db, 15).await {
            eprintln!("sync failed: {error:#}");
        }
        // Uploading blocks on the network; let the runtime move other work off this thread.
        if let Err(error) = tokio::task::block_in_place(|| upload(db, server, &token)) {
            eprintln!("upload failed: {error:#}");
        }
        tokio::time::sleep(pause).await;
    }
}

fn token() -> Result<String> {
    std::env::var("HEALTHSTRAP_TOKEN").context("set HEALTHSTRAP_TOKEN to the server's API token")
}

fn pick(found: Vec<ble::Found>, wanted: Option<&str>) -> Result<ble::Found> {
    let mut matches: Vec<_> = found
        .into_iter()
        .filter(|f| wanted.is_none_or(|w| f.id == w || f.name.as_deref() == Some(w)))
        .collect();
    match matches.len() {
        0 => {
            bail!("no strap found; take it off the charger, keep it close and close the WHOOP app")
        }
        1 => Ok(matches.remove(0)),
        _ => bail!("several straps found; pick one with --device (see `healthstrap scan`)"),
    }
}
