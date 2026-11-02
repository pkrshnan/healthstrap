//! Bluetooth: finding the strap, connecting, and moving bytes. Works on macOS and Linux.

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
    /// The handle used to connect.
    pub peripheral: Peripheral,
}

/// Scans for straps advertising the WHOOP 5.0 service.
pub async fn scan(duration: Duration) -> Result<Vec<Found>> {
    let adapter = first_adapter().await?;
    let service = parse_uuid(gatt::SERVICE)?;
    adapter
        .start_scan(ScanFilter {
            services: vec![service],
        })
        .await?;
    tokio::time::sleep(duration).await;
    adapter.stop_scan().await?;

    let mut found = Vec::new();
    for peripheral in adapter.peripherals().await? {
        let properties = peripheral.properties().await?.unwrap_or_default();
        if properties.services.contains(&service) || peripheral.is_connected().await? {
            found.push(Found {
                id: peripheral.id().to_string(),
                name: properties.local_name,
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
}

impl Link {
    /// Connects, discovers services and subscribes to every notify characteristic.
    pub async fn open(peripheral: Peripheral) -> Result<Self> {
        peripheral.connect().await.context("connecting")?;
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
            }
        }
        Ok(Self {
            peripheral,
            command,
        })
    }

    /// Every notification's bytes, from all subscribed characteristics, in arrival order.
    pub async fn notifications(&self) -> Result<impl Stream<Item = Vec<u8>> + use<>> {
        Ok(self.peripheral.notifications().await?.map(|n| n.value))
    }

    /// Writes one command frame and waits for the write to be confirmed.
    pub async fn send(&self, frame: &[u8]) -> Result<()> {
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

async fn first_adapter() -> Result<Adapter> {
    let manager = Manager::new().await.context("starting Bluetooth")?;
    let Some(adapter) = manager.adapters().await?.into_iter().next() else {
        bail!("no Bluetooth adapter found");
    };
    Ok(adapter)
}

fn parse_uuid(text: &str) -> Result<Uuid> {
    Uuid::parse_str(text).with_context(|| format!("bad UUID {text}"))
}
