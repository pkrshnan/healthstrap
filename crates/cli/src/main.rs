//! `healthstrap`: sync a WHOOP 5.0 strap over Bluetooth on macOS and Linux.

mod ble;
mod store;
mod sync;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
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
        } => {
            let mut store = store::Store::open(&db)?;
            let strap = pick(
                ble::scan(Duration::from_secs(seconds)).await?,
                device.as_deref(),
            )?;
            println!("syncing {}", strap.id);
            let link = ble::Link::open(strap.peripheral).await?;
            let result = sync::run(&link, &mut store, &strap.id).await;
            link.close().await;
            let summary = result?;
            println!(
                "{} chunks acknowledged, {} new frames, {}",
                summary.chunks,
                summary.new_frames,
                if summary.complete {
                    "history complete"
                } else {
                    "stopped early; run again"
                }
            );
        }
    }
    Ok(())
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
