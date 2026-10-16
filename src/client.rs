//! BLE client for the ring (btleplug → BlueZ on Linux), mirroring LinkTransport.
//!
//! Read-only by default. Writes: [`Index01Client::program`], [`Index01Client::set_time`],
//! [`Index01Client::program_user`] and [`Index01Client::erase_stored_collection_indexes`]
//! (needs an explicit confirmation argument; CLI: `index01 erase --yes`).

use std::pin::Pin;
use std::time::Duration;

use btleplug::api::{
    BDAddr, Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::{FutureExt, Stream, StreamExt};
use tokio::time::{sleep, Instant};

use crate::protocol::{
    self as p, AdvertisementData, StoredCollectionIndexes, TelestoOp, TelestoRequest,
};
use crate::telesto::{run_operation, Link, Notification, OpError, DEFAULT_TIMEOUT};

/// Timeout for streaming one whole collection.
pub const COLLECTION_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("bluetooth: {0}")]
    Ble(#[from] btleplug::Error),
    #[error("no bluetooth adapter found")]
    NoAdapter,
    #[error("invalid address {0:?}")]
    BadAddress(String),
    #[error("ring {0} not found (press the button to wake it)")]
    NotFound(BDAddr),
    #[error("haversine service not found on device")]
    ServiceMissing,
    #[error("characteristic {0} not found")]
    CharacteristicMissing(uuid::Uuid),
    #[error(transparent)]
    Op(#[from] OpError<btleplug::Error>),
    #[error(transparent)]
    Parse(#[from] p::ParseError),
    #[error("read-back after PROGRAM did not match")]
    VerifyFailed,
    #[error("refusing to erase without explicit confirmation")]
    EraseNotConfirmed,
}

pub type Result<T> = std::result::Result<T, ClientError>;

pub async fn default_adapter() -> Result<Adapter> {
    Manager::new()
        .await?
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or(ClientError::NoAdapter)
}

/// One advertising ring seen during a scan.
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub address: BDAddr,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub services: Vec<uuid::Uuid>,
    /// Manufacturer data for company 0x0EEA, without the company id.
    pub manufacturer_data: Option<Vec<u8>>,
    pub advertisement: Option<AdvertisementData>,
}

/// Scan for rings advertising the Haversine service for `duration`.
pub async fn scan(adapter: &Adapter, duration: Duration) -> Result<Vec<ScanResult>> {
    adapter
        .start_scan(ScanFilter {
            services: vec![p::HAVERSINE_SERVICE_128],
        })
        .await?;
    sleep(duration).await;
    adapter.stop_scan().await?;
    let mut out = Vec::new();
    for per in adapter.peripherals().await? {
        let Some(props) = per.properties().await? else {
            continue;
        };
        let mdata = props
            .manufacturer_data
            .get(&p::HAVERSINE_COMPANY_ID)
            .cloned();
        if mdata.is_none() && !props.services.contains(&p::HAVERSINE_SERVICE_128) {
            continue;
        }
        out.push(ScanResult {
            address: props.address,
            name: props.local_name,
            rssi: props.rssi,
            services: props.services,
            advertisement: mdata
                .as_deref()
                .and_then(|m| p::parse_advertisement(m).ok()),
            manufacturer_data: mdata,
        });
    }
    out.sort_by_key(|r| r.address);
    Ok(out)
}

/// Scan until the peripheral with `address` shows up, or `timeout`.
pub async fn find(adapter: &Adapter, address: BDAddr, timeout: Duration) -> Result<Peripheral> {
    adapter
        .start_scan(ScanFilter {
            services: vec![p::HAVERSINE_SERVICE_128],
        })
        .await?;
    let deadline = Instant::now() + timeout;
    let found = loop {
        if let Some(per) = adapter
            .peripherals()
            .await?
            .into_iter()
            .find(|per| per.address() == address)
        {
            break Some(per);
        }
        if Instant::now() >= deadline {
            break None;
        }
        sleep(Duration::from_millis(250)).await;
    };
    adapter.stop_scan().await?;
    found.ok_or(ClientError::NotFound(address))
}

/// Wait for an advertisement from `address` that satisfies `pred`, without connecting.
/// Scans continuously (BlueZ reports manufacturer-data changes as they arrive).
pub async fn wait_for_advert(
    adapter: &Adapter,
    address: BDAddr,
    mut pred: impl FnMut(&AdvertisementData) -> bool,
) -> Result<AdvertisementData> {
    use btleplug::api::CentralEvent;
    let mut events = adapter.events().await?;
    adapter
        .start_scan(ScanFilter {
            services: vec![p::HAVERSINE_SERVICE_128],
        })
        .await?;
    let found = loop {
        let Some(ev) = events.next().await else {
            break None;
        };
        // Only live advertisement payloads: DeviceUpdated/properties() can replay BlueZ's
        // cached manufacturer data from before an erase.
        let CentralEvent::ManufacturerDataAdvertisement {
            id,
            manufacturer_data,
        } = ev
        else {
            continue;
        };
        let Ok(per) = adapter.peripheral(&id).await else {
            continue;
        };
        if per.address() != address {
            continue;
        }
        let Some(adv) = manufacturer_data
            .get(&p::HAVERSINE_COMPANY_ID)
            .and_then(|m| p::parse_advertisement(m).ok())
        else {
            continue;
        };
        if pred(&adv) {
            break Some(adv);
        }
    };
    adapter.stop_scan().await?;
    found.ok_or(ClientError::NotFound(address))
}

type NotificationStream = Pin<Box<dyn Stream<Item = btleplug::api::ValueNotification> + Send>>;

/// The ctrl/data characteristic pair of a connected ring.
pub struct BleLink {
    peripheral: Peripheral,
    ctrl: Characteristic,
    data: Characteristic,
    notifications: NotificationStream,
    chunk: usize,
}

impl Link for BleLink {
    type Error = btleplug::Error;

    async fn write_ctrl(&mut self, bytes: &[u8]) -> std::result::Result<(), btleplug::Error> {
        self.peripheral
            .write(&self.ctrl, bytes, WriteType::WithoutResponse)
            .await
    }

    async fn write_data(&mut self, bytes: &[u8]) -> std::result::Result<(), btleplug::Error> {
        self.peripheral
            .write(&self.data, bytes, WriteType::WithoutResponse)
            .await
    }

    async fn next_notification(&mut self) -> Option<Notification> {
        loop {
            let n = self.notifications.next().await?;
            if n.uuid == p::TELESTO_CTRL_UUID {
                return Some(Notification::Ctrl(n.value));
            }
            if n.uuid == p::TELESTO_DATA_UUID {
                return Some(Notification::Data(n.value));
            }
        }
    }

    fn drain(&mut self) {
        while let Some(Some(_)) = self.notifications.next().now_or_never() {}
    }

    fn chunk_size(&self) -> usize {
        self.chunk
    }
}

/// Async Telesto client.
pub struct Index01Client {
    link: BleLink,
}

impl Index01Client {
    /// Find the ring by address (scanning up to `scan_timeout`) and connect.
    pub async fn connect_address(address: &str, scan_timeout: Duration) -> Result<Self> {
        let addr =
            BDAddr::from_str_delim(address).map_err(|_| ClientError::BadAddress(address.into()))?;
        let adapter = default_adapter().await?;
        let per = find(&adapter, addr, scan_timeout).await?;
        Self::connect(per).await
    }

    /// Connect, check for the Haversine service and subscribe ctrl then data (LinkTransport
    /// order). The systemInput CCCD is not enabled; the app only uses it as a disconnect signal.
    /// Bonding is left to BlueZ: the ring must already be paired with this adapter.
    pub async fn connect(peripheral: Peripheral) -> Result<Self> {
        if !peripheral.is_connected().await? {
            peripheral.connect().await?;
        }
        peripheral.discover_services().await?;
        let services = peripheral.services();
        if !services
            .iter()
            .any(|s| s.uuid == p::HAVERSINE_SERVICE_128 || s.uuid == p::HAVERSINE_SERVICE_16)
        {
            return Err(ClientError::ServiceMissing);
        }
        let chars = peripheral.characteristics();
        let get = |u| {
            chars
                .iter()
                .find(|c| c.uuid == u)
                .cloned()
                .ok_or(ClientError::CharacteristicMissing(u))
        };
        let (ctrl, data) = (get(p::TELESTO_CTRL_UUID)?, get(p::TELESTO_DATA_UUID)?);
        peripheral.subscribe(&ctrl).await?;
        peripheral.subscribe(&data).await?;
        let notifications = peripheral.notifications().await?;
        let chunk = (peripheral.mtu() as usize).saturating_sub(3).max(20);
        Ok(Self {
            link: BleLink {
                peripheral,
                ctrl,
                data,
                notifications,
                chunk,
            },
        })
    }

    pub async fn disconnect(self) -> Result<()> {
        let per = &self.link.peripheral;
        let _ = per.unsubscribe(&self.link.ctrl).await;
        let _ = per.unsubscribe(&self.link.data).await;
        per.disconnect().await?;
        Ok(())
    }

    pub fn peripheral(&self) -> &Peripheral {
        &self.link.peripheral
    }

    /// Run one raw Telesto operation. PROGRAM and ERASE write to the ring.
    pub async fn run_operation(
        &mut self,
        request: TelestoRequest,
        payload: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<(p::CtrlResponse, Vec<u8>)> {
        Ok(run_operation(&mut self.link, request, payload, timeout).await?)
    }

    pub async fn read(&mut self, address: u32, length: u32, timeout: Duration) -> Result<Vec<u8>> {
        Ok(self
            .run_operation(TelestoRequest::read(address, length), None, timeout)
            .await?
            .1)
    }

    pub async fn read_platform_versions(&mut self) -> Result<Vec<u8>> {
        self.read(p::PLATFORM_VERSIONS, 4, DEFAULT_TIMEOUT).await
    }

    pub async fn read_serial_number(&mut self) -> Result<Vec<u8>> {
        self.read(p::SERIAL_NUMBER_ADDR, 6, DEFAULT_TIMEOUT).await
    }

    pub async fn read_app_data(&mut self, max_len: u32) -> Result<Vec<u8>> {
        self.read(p::APPLICATION_DATA_STORE, max_len, DEFAULT_TIMEOUT)
            .await
    }

    pub async fn read_stored_collection_indexes(&mut self) -> Result<StoredCollectionIndexes> {
        let raw = self
            .read(p::STORED_COLLECTION_INDEXES, 4, DEFAULT_TIMEOUT)
            .await?;
        Ok(p::parse_stored_collection_indexes(&raw)?)
    }

    /// Stream collection `index`. Length 0 means "until the ring ends the transfer", like
    /// the native transfer loop.
    pub async fn read_collection(&mut self, index: u16) -> Result<Vec<u8>> {
        self.read(p::collection_address(index as u32), 0, COLLECTION_TIMEOUT)
            .await
    }

    /// PROGRAM `payload` at `address`. Writes to the ring.
    pub async fn program(&mut self, address: u32, payload: &[u8]) -> Result<()> {
        let req = TelestoRequest::new(TelestoOp::ProgramMemory, address, 0, payload.len() as u32);
        self.run_operation(req, Some(payload), Duration::from_secs(30))
            .await?;
        Ok(())
    }

    /// Set the ring clock (the app's service step: PROGRAM unix time to 0x40030001).
    /// The ring resets its clock when it wakes from its dark state; recordings are
    /// timestamped from this clock.
    pub async fn set_time(&mut self, unix_time: u32) -> Result<()> {
        self.program(p::UNIX_TIME_ADDR, &unix_time.to_le_bytes())
            .await
    }

    /// Program the user record (ProgramApplicationDataOperation: length-prefixed
    /// `PPRingApplicationData` to 0x40000000). Writes to the ring.
    pub async fn program_user(&mut self, user_id: &str, unix_time: u32) -> Result<()> {
        let blob = crate::appdata::build_app_data(user_id, unix_time);
        let mut prefixed = ((blob.len() + 4) as u32).to_le_bytes().to_vec();
        prefixed.extend_from_slice(&blob);
        self.program(p::APPLICATION_DATA_STORE, &prefixed).await?;
        let back = self
            .read(
                p::APPLICATION_DATA_STORE,
                prefixed.len() as u32,
                DEFAULT_TIMEOUT,
            )
            .await?;
        if back != prefixed {
            return Err(ClientError::VerifyFailed);
        }
        Ok(())
    }

    /// ERASE 0x40030000 len 0x200 (HaversineEraseCollectionsOperation_init). Destructive.
    pub async fn erase_stored_collection_indexes(
        &mut self,
        i_understand_this_erases: bool,
    ) -> Result<()> {
        if !i_understand_this_erases {
            return Err(ClientError::EraseNotConfirmed);
        }
        let req = TelestoRequest::new(TelestoOp::EraseMemory, p::CONFIG_BASE, 0, 0x200);
        self.run_operation(req, None, DEFAULT_TIMEOUT).await?;
        Ok(())
    }
}
