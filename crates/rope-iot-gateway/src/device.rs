//! Device registry — maps IoT device identities to wallet addresses on Rope.

use hashbrown::HashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceType {
    Sensor,
    Actuator,
    Gateway,
    Camera,
    Meter,
    Vehicle,
    Wearable,
    Custom(String),
}

impl DeviceType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Sensor => "sensor",
            Self::Actuator => "actuator",
            Self::Gateway => "gateway",
            Self::Camera => "camera",
            Self::Meter => "meter",
            Self::Vehicle => "vehicle",
            Self::Wearable => "wearable",
            Self::Custom(s) => s,
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "sensor" => Self::Sensor,
            "actuator" => Self::Actuator,
            "gateway" => Self::Gateway,
            "camera" => Self::Camera,
            "meter" => Self::Meter,
            "vehicle" => Self::Vehicle,
            "wearable" => Self::Wearable,
            other => Self::Custom(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceStatus {
    Online,
    Offline,
    Maintenance,
    Decommissioned,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub wallet_address: String,
    pub device_type: DeviceType,
    pub name: String,
    pub location: Option<DeviceLocation>,
    pub firmware_version: Option<String>,
    pub owner_wallet: String,
    pub status: DeviceStatus,
    pub registered_at: i64,
    pub last_seen_at: i64,
    pub telemetry_count: u64,
    pub metadata: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceLocation {
    pub lat: f64,
    pub lng: f64,
    pub altitude: Option<f64>,
    pub label: Option<String>,
}

/// Thread-safe device registry
pub struct DeviceRegistry {
    devices: RwLock<HashMap<String, DeviceInfo>>,
    wallet_to_device: RwLock<HashMap<String, String>>,
}

impl DeviceRegistry {
    pub fn new() -> Self {
        Self {
            devices: RwLock::new(HashMap::new()),
            wallet_to_device: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, info: DeviceInfo) -> Result<(), String> {
        let device_id = info.device_id.clone();
        let wallet = info.wallet_address.clone();

        if self.devices.read().contains_key(&device_id) {
            return Err(format!("Device {} already registered", device_id));
        }

        self.wallet_to_device
            .write()
            .insert(wallet, device_id.clone());
        self.devices.write().insert(device_id, info);
        Ok(())
    }

    pub fn get_by_id(&self, device_id: &str) -> Option<DeviceInfo> {
        self.devices.read().get(device_id).cloned()
    }

    pub fn get_by_wallet(&self, wallet: &str) -> Option<DeviceInfo> {
        let device_id = self.wallet_to_device.read().get(wallet).cloned()?;
        self.devices.read().get(&device_id).cloned()
    }

    pub fn record_telemetry(&self, wallet: &str) {
        if let Some(device_id) = self.wallet_to_device.read().get(wallet).cloned() {
            if let Some(device) = self.devices.write().get_mut(&device_id) {
                device.telemetry_count += 1;
                device.last_seen_at = chrono::Utc::now().timestamp();
                device.status = DeviceStatus::Online;
            }
        }
    }

    pub fn update_status(&self, device_id: &str, status: DeviceStatus) {
        if let Some(device) = self.devices.write().get_mut(device_id) {
            device.status = status;
        }
    }

    pub fn list_devices(&self) -> Vec<DeviceInfo> {
        self.devices.read().values().cloned().collect()
    }

    pub fn device_count(&self) -> usize {
        self.devices.read().len()
    }

    pub fn online_count(&self) -> usize {
        self.devices
            .read()
            .values()
            .filter(|d| d.status == DeviceStatus::Online)
            .count()
    }

    pub fn deregister(&self, device_id: &str) -> bool {
        let mut devices = self.devices.write();
        if let Some(info) = devices.remove(device_id) {
            self.wallet_to_device.write().remove(&info.wallet_address);
            true
        } else {
            false
        }
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}
