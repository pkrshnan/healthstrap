//! Bluetooth: finding the strap, connecting, and moving bytes. Works on macOS and Linux.

use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::{Stream, StreamExt};
use healthstrap_core::gatt;
use uuid::Uuid;

/// A strap seen during a scan.
pub struct Found {
    /// Platform identifier: a MAC address on Linux, a UUID on macOS.
    pub id: String,
    /// Advertised name, if any.
    pub name: Option<String>,
    /// Whether this computer already holds a connection to it.
    pub connected: bool,
    /// The handle used to connect.
    pub peripheral: Peripheral,
}

/// Scans for nearby straps. With `everything`, returns every device seen, for troubleshooting.
///
/// The scan itself is unfiltered, and a device counts as a strap if it advertises the WHOOP 5.0
/// service or its name contains "whoop". Some straps only show their name, and a strap `BlueZ`
/// already knows may list its services without advertising them.
pub async fn scan(duration: Duration, everything: bool) -> Result<Vec<Found>> {
    let adapter = first_adapter().await?;
    let service = parse_uuid(gatt::SERVICE)?;
    adapter.start_scan(ScanFilter::default()).await?;
    tokio::time::sleep(duration).await;
    // BlueZ answers "No discovery started" if discovery already ended on its own. The results
    // gathered so far are still valid, so a failed stop is not an error.
    if let Err(error) = adapter.stop_scan().await {
        eprintln!("note: stopping the scan: {error}");
    }

    let mut found = Vec::new();
    for peripheral in adapter.peripherals().await? {
        let properties = peripheral.properties().await?.unwrap_or_default();
        let named_whoop = properties
            .local_name
            .as_deref()
            .is_some_and(|name| name.to_lowercase().contains("whoop"));
        if everything || named_whoop || properties.services.contains(&service) {
            found.push(Found {
                id: peripheral.id().to_string(),
                name: properties.local_name,
                connected: peripheral.is_connected().await?,
                peripheral,
            });
        }
    }
    Ok(found)
}

/// A connected strap with its notifications subscribed.
pub struct Link {
    peripheral: Peripheral,
    command: Characteristic,
    debug: bool,
}

impl Link {
    /// Connects, discovers services and subscribes to every notify characteristic.
    /// With `debug`, every byte sent and received is printed to stderr as hex.
    pub async fn open(peripheral: Peripheral, debug: bool) -> Result<Self> {
        connect(&peripheral).await?;
        peripheral
            .discover_services()
            .await
            .context("discovering services")?;

        let characteristics = peripheral.characteristics();
        let command_uuid = parse_uuid(gatt::COMMAND_WRITE)?;
        let command = characteristics
            .iter()
            .find(|c| c.uuid == command_uuid)
            .cloned()
            .ok_or_else(|| anyhow!("strap has no command characteristic; is it a WHOOP 5.0?"))?;

        for uuid in gatt::NOTIFY {
            let uuid = parse_uuid(uuid)?;
            if let Some(characteristic) = characteristics.iter().find(|c| c.uuid == uuid) {
                peripheral.subscribe(characteristic).await?;
                if debug {
                    eprintln!("subscribed {uuid}");
                }
            } else if debug {
                eprintln!("missing {uuid}");
            }
        }
        Ok(Self {
            peripheral,
            command,
            debug,
        })
    }

    /// Every notification's bytes, from all subscribed characteristics, in arrival order.
    pub async fn notifications(&self) -> Result<impl Stream<Item = Vec<u8>> + use<>> {
        let debug = self.debug;
        Ok(self
            .peripheral
            .notifications()
            .await?
            .map(move |notification| {
                if debug {
                    eprintln!("<- {} {}", notification.uuid, hex(&notification.value));
                }
                notification.value
            }))
    }

    /// Writes one command frame and waits for the write to be confirmed.
    pub async fn send(&self, frame: &[u8]) -> Result<()> {
        if self.debug {
            eprintln!("-> {}", hex(frame));
        }
        self.peripheral
            .write(&self.command, frame, WriteType::WithResponse)
            .await
            .context("writing command")
    }

    /// Disconnects. Errors are ignored: the link may already be gone.
    pub async fn close(self) {
        let _ = self.peripheral.disconnect().await;
    }
}

/// How many times to try connecting before giving up.
const CONNECT_ATTEMPTS: u32 = 4;

/// Connects, retrying with growing pauses. A strap that just lost a connection can refuse new
/// ones for a while, which `BlueZ` reports as `le-connection-abort-by-local`.
async fn connect(peripheral: &Peripheral) -> Result<()> {
    let mut pause = Duration::from_secs(5);
    let mut attempt = 1;
    loop {
        match peripheral.connect().await {
            Ok(()) => return Ok(()),
            Err(error) if attempt < CONNECT_ATTEMPTS => {
                eprintln!(
                    "connecting failed ({error}), retrying in {}s",
                    pause.as_secs()
                );
                tokio::time::sleep(pause).await;
                pause *= 2;
                attempt += 1;
            }
            Err(error) => return Err(error).context("connecting"),
        }
    }
}

async fn first_adapter() -> Result<Adapter> {
    let manager = Manager::new().await.context("starting Bluetooth")?;
    let Some(adapter) = manager.adapters().await?.into_iter().next() else {
        bail!("no Bluetooth adapter found");
    };
    Ok(adapter)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn parse_uuid(text: &str) -> Result<Uuid> {
    Uuid::parse_str(text).with_context(|| format!("bad UUID {text}"))
}
