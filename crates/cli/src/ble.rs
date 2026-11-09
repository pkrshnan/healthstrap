//! Bluetooth: finding the strap, connecting, and moving bytes. Works on macOS and Linux.

use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use btleplug::api::{
    Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, PeripheralProperties,
    ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral, PeripheralId};
use futures::{Stream, StreamExt};
use healthstrap_core::gatt;
use tokio::sync::OnceCell;
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
        if everything || is_strap(&properties, &service) {
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

/// Listens for up to `limit` for a strap that is advertising right now, which means it is in
/// range and free to connect. Returns `None` if none was heard. With `wanted`, only the strap
/// with that id or name counts.
///
/// `BlueZ` remembers paired devices and lists them even when they are far away, so a device in
/// its cache does not count: only a fresh advertisement, or a device with a signal strength from
/// this scan, does.
pub async fn wait_for_strap(wanted: Option<&str>, limit: Duration) -> Result<Option<Found>> {
    let adapter = first_adapter().await?;
    let service = parse_uuid(gatt::SERVICE)?;
    let mut events = adapter.events().await?;
    adapter.start_scan(ScanFilter::default()).await?;
    let heard = tokio::time::timeout(limit, async {
        while let Some(event) = events.next().await {
            let Some((id, live)) = heard_from(event) else {
                continue;
            };
            let peripheral = adapter.peripheral(&id).await?;
            let properties = peripheral.properties().await?.unwrap_or_default();
            let nearby = live || properties.rssi.is_some();
            if nearby
                && is_strap(&properties, &service)
                && is_wanted(&peripheral, &properties, wanted)
            {
                return Ok::<_, anyhow::Error>(Some(Found {
                    id: peripheral.id().to_string(),
                    name: properties.local_name,
                    connected: peripheral.is_connected().await?,
                    peripheral,
                }));
            }
        }
        Ok(None)
    })
    .await;
    // Same as in `scan`: a scan that already ended is fine.
    let _ = adapter.stop_scan().await;
    heard.unwrap_or(Ok(None))
}

/// The device an event is about, and whether the event can only come from a live
/// advertisement. Discovery events can also be replayed from `BlueZ`'s cache, so they are not.
fn heard_from(event: CentralEvent) -> Option<(PeripheralId, bool)> {
    match event {
        CentralEvent::RssiUpdate { id, .. }
        | CentralEvent::ManufacturerDataAdvertisement { id, .. }
        | CentralEvent::ServiceDataAdvertisement { id, .. } => Some((id, true)),
        CentralEvent::DeviceDiscovered(id)
        | CentralEvent::DeviceUpdated(id)
        | CentralEvent::ServicesAdvertisement { id, .. } => Some((id, false)),
        CentralEvent::DeviceConnected(_)
        | CentralEvent::DeviceDisconnected(_)
        | CentralEvent::DeviceServicesModified(_)
        | CentralEvent::StateUpdate(_) => None,
    }
}

/// A device counts as a strap if it advertises the WHOOP 5.0 service or its name contains
/// "whoop".
fn is_strap(properties: &PeripheralProperties, service: &Uuid) -> bool {
    let named_whoop = properties
        .local_name
        .as_deref()
        .is_some_and(|name| name.to_lowercase().contains("whoop"));
    named_whoop || properties.services.contains(service)
}

fn is_wanted(
    peripheral: &Peripheral,
    properties: &PeripheralProperties,
    wanted: Option<&str>,
) -> bool {
    wanted.is_none_or(|w| {
        peripheral.id().to_string() == w || properties.local_name.as_deref() == Some(w)
    })
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
        // Without a `Link` nothing would close the connection, and a link left up blocks every
        // later connect, so drop it before giving up.
        match prepare(&peripheral, debug).await {
            Ok(command) => Ok(Self {
                peripheral,
                command,
                debug,
            }),
            Err(error) => {
                unsubscribe_all(&peripheral).await;
                let _ = peripheral.disconnect().await;
                Err(error)
            }
        }
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

    /// Unsubscribes and disconnects. Errors are ignored: the link may already be gone.
    pub async fn close(self) {
        unsubscribe_all(&self.peripheral).await;
        let _ = self.peripheral.disconnect().await;
    }
}

/// Discovers services and subscribes to every notify characteristic, returning the command one.
async fn prepare(peripheral: &Peripheral, debug: bool) -> Result<Characteristic> {
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
    Ok(command)
}

/// Ends this process's notification subscriptions. `BlueZ` keeps a subscription across
/// disconnects and turns it back on at the next connect, so the next sync starts from none
/// rather than relying on how `BlueZ` treats a second subscribe. Errors are ignored.
async fn unsubscribe_all(peripheral: &Peripheral) {
    for characteristic in peripheral.characteristics() {
        if gatt::NOTIFY
            .iter()
            .any(|uuid| parse_uuid(uuid).is_ok_and(|uuid| uuid == characteristic.uuid))
        {
            let _ = peripheral.unsubscribe(&characteristic).await;
        }
    }
}

/// How many times to try connecting before giving up.
const CONNECT_ATTEMPTS: u32 = 4;

/// Connects, retrying with growing pauses. A strap that just lost a connection can refuse new
/// ones for a while, which `BlueZ` reports as `le-connection-abort-by-local`.
///
/// A failed attempt can leave `BlueZ` holding the link with its services never resolved, and
/// connecting again over that link times out the same way ("Service discovery timed out")
/// until something drops it. So every failed attempt drops its link, and a link left over from
/// an earlier run, such as a daemon that crashed, is dropped before the first.
async fn connect(peripheral: &Peripheral) -> Result<()> {
    if peripheral.is_connected().await.unwrap_or(false) {
        eprintln!("dropping a connection left from an earlier run");
        let _ = peripheral.disconnect().await;
    }
    let mut pause = Duration::from_secs(5);
    let mut attempt = 1;
    loop {
        let result = peripheral.connect().await;
        if result.is_err() {
            let _ = peripheral.disconnect().await;
        }
        match result {
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

/// The adapter, opened once and shared by every scan and sync in this process.
///
/// On Linux each `Manager` opens its own D-Bus connection, which btleplug never closes, and
/// `BlueZ` keeps a notification subscription alive for every connection that ever subscribed,
/// restoring it on each reconnect. With a new manager per sync, every notification arrived once
/// more for each earlier sync, so the daemon acknowledged each chunk many times over until the
/// link dropped mid-write, which `BlueZ` reports as ATT error 0x0e.
static ADAPTER: OnceCell<Adapter> = OnceCell::const_new();

async fn first_adapter() -> Result<Adapter> {
    ADAPTER
        .get_or_try_init(|| async {
            let manager = Manager::new().await.context("starting Bluetooth")?;
            let Some(adapter) = manager.adapters().await?.into_iter().next() else {
                bail!("no Bluetooth adapter found");
            };
            Ok(adapter)
        })
        .await
        .cloned()
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
