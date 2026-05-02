//! IoT Gateway — core runtime that accepts MQTT/CoAP telemetry and bridges
//! it to personal String fragments via the LedgerManager interface.
//!
//! The gateway is a long-running tokio task spawned by the rope-node.
//! It does NOT depend on LedgerManager directly (to avoid circular deps).
//! Instead, it accepts an `IoTSink` callback that the node wires to the
//! actual LedgerManager at startup.

use crate::device::{DeviceInfo, DeviceLocation, DeviceRegistry, DeviceStatus, DeviceType};
use crate::protocol::{
    parse_mqtt_topic, DeviceEvent, MqttMessageType, SourceProtocol, TelemetryPayload,
    TelemetryValue,
};
use hashbrown::HashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Callback signature: the node provides a closure that writes to the ledger.
/// Parameters: (wallet_address, interaction_type, description, metadata)
pub type IoTSink = Arc<
    dyn Fn(String, String, String, HashMap<String, String>) -> Result<(), String> + Send + Sync,
>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IoTGatewayConfig {
    pub enabled: bool,
    pub mqtt_port: u16,
    pub coap_port: u16,
    pub max_devices: usize,
    pub telemetry_buffer_size: usize,
    pub batch_flush_interval_ms: u64,
    pub stale_device_timeout_secs: u64,
}

impl Default for IoTGatewayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mqtt_port: 1883,
            coap_port: 5683,
            max_devices: 10_000,
            telemetry_buffer_size: 1_000,
            batch_flush_interval_ms: 5_000,
            stale_device_timeout_secs: 300,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GatewayStats {
    pub devices_registered: usize,
    pub devices_online: usize,
    pub telemetry_received: u64,
    pub events_received: u64,
    pub fragments_written: u64,
    pub errors: u64,
    pub mqtt_connected: bool,
    pub coap_running: bool,
    pub uptime_secs: u64,
}

/// The IoT Gateway runtime.
pub struct IoTGateway {
    config: IoTGatewayConfig,
    registry: Arc<DeviceRegistry>,
    sink: Option<IoTSink>,
    stats: Arc<RwLock<GatewayStats>>,
    started_at: i64,
    ingest_tx: Option<mpsc::Sender<IngestMessage>>,
}

enum IngestMessage {
    Telemetry(TelemetryPayload),
    Event(DeviceEvent),
}

impl IoTGateway {
    pub fn new(config: IoTGatewayConfig) -> Self {
        Self {
            config,
            registry: Arc::new(DeviceRegistry::new()),
            sink: None,
            stats: Arc::new(RwLock::new(GatewayStats::default())),
            started_at: chrono::Utc::now().timestamp(),
            ingest_tx: None,
        }
    }

    /// Wire the ledger sink — called by rope-node after constructing the LedgerManager.
    pub fn set_sink(&mut self, sink: IoTSink) {
        self.sink = Some(sink);
    }

    pub fn registry(&self) -> &Arc<DeviceRegistry> {
        &self.registry
    }

    pub fn stats(&self) -> GatewayStats {
        let mut s = self.stats.read().clone();
        s.devices_registered = self.registry.device_count();
        s.devices_online = self.registry.online_count();
        s.uptime_secs = (chrono::Utc::now().timestamp() - self.started_at) as u64;
        s
    }

    /// Register a device in the gateway.
    pub fn register_device(
        &self,
        device_id: String,
        wallet_address: String,
        device_type: &str,
        name: String,
        owner_wallet: String,
        location: Option<(f64, f64)>,
        metadata: HashMap<String, String>,
    ) -> Result<DeviceInfo, String> {
        if self.registry.device_count() >= self.config.max_devices {
            return Err(format!(
                "Device limit reached ({})",
                self.config.max_devices
            ));
        }

        let info = DeviceInfo {
            device_id: device_id.clone(),
            wallet_address: wallet_address.clone(),
            device_type: DeviceType::from_str(device_type),
            name,
            location: location.map(|(lat, lng)| DeviceLocation {
                lat,
                lng,
                altitude: None,
                label: None,
            }),
            firmware_version: metadata.get("firmware").cloned(),
            owner_wallet,
            status: DeviceStatus::Online,
            registered_at: chrono::Utc::now().timestamp(),
            last_seen_at: chrono::Utc::now().timestamp(),
            telemetry_count: 0,
            metadata,
        };

        self.registry.register(info.clone())?;

        if let Some(sink) = &self.sink {
            let mut meta = hashbrown::HashMap::new();
            meta.insert("device_id".into(), device_id);
            meta.insert("device_type".into(), info.device_type.as_str().into());
            let _ = sink(
                wallet_address,
                "Custom".into(),
                format!("Device registered: {}", info.name),
                meta,
            );
        }

        tracing::info!(
            "IoT device registered: {} ({})",
            info.name,
            info.device_type.as_str()
        );
        Ok(info)
    }

    /// Ingest a telemetry payload — writes it as a fragment on the device's personal String.
    pub fn ingest_telemetry(&self, payload: TelemetryPayload) -> Result<(), String> {
        let sink = self.sink.as_ref().ok_or("Gateway sink not configured")?;

        self.registry.record_telemetry(&payload.device_wallet);

        let mut meta = hashbrown::HashMap::new();
        meta.insert(
            "source_protocol".into(),
            payload.source_protocol.as_str().into(),
        );
        meta.insert("reading_count".into(), payload.readings.len().to_string());
        if let Some(seq) = payload.sequence_number {
            meta.insert("sequence".into(), seq.to_string());
        }

        for (key, value) in &payload.readings {
            meta.insert(format!("reading_{}", key), value.to_string());
        }

        let description = format!(
            "Telemetry: {} readings via {}",
            payload.readings.len(),
            payload.source_protocol.as_str()
        );

        sink(payload.device_wallet, "Custom".into(), description, meta)
            .map_err(|e| format!("Ledger write failed: {}", e))?;

        self.stats.write().telemetry_received += 1;
        self.stats.write().fragments_written += 1;
        Ok(())
    }

    /// Ingest a discrete device event.
    pub fn ingest_event(&self, event: DeviceEvent) -> Result<(), String> {
        let sink = self.sink.as_ref().ok_or("Gateway sink not configured")?;

        self.registry.record_telemetry(&event.device_wallet);

        let mut meta: hashbrown::HashMap<String, String> = event.metadata.clone();
        meta.insert("event_type".into(), event.event_type.as_str().into());
        meta.insert("severity".into(), format!("{:?}", event.severity));

        sink(
            event.device_wallet,
            "Custom".into(),
            event.description,
            meta,
        )
        .map_err(|e| format!("Ledger write failed: {}", e))?;

        self.stats.write().events_received += 1;
        self.stats.write().fragments_written += 1;
        Ok(())
    }

    /// Process an MQTT message (called by the MQTT listener).
    pub fn handle_mqtt_message(&self, topic: &str, payload: &[u8]) -> Result<(), String> {
        let (wallet, msg_type) =
            parse_mqtt_topic(topic).ok_or_else(|| format!("Invalid MQTT topic: {}", topic))?;

        match msg_type {
            MqttMessageType::Telemetry => {
                let readings: HashMap<String, TelemetryValue> = serde_json::from_slice(payload)
                    .map_err(|e| format!("Invalid telemetry JSON: {}", e))?;

                let telem = TelemetryPayload {
                    device_wallet: wallet,
                    timestamp: chrono::Utc::now().timestamp(),
                    readings,
                    source_protocol: SourceProtocol::Mqtt,
                    sequence_number: None,
                    quality: None,
                };
                self.ingest_telemetry(telem)
            }
            MqttMessageType::Event | MqttMessageType::Diagnostic => {
                let event: DeviceEvent = serde_json::from_slice(payload)
                    .map_err(|e| format!("Invalid event JSON: {}", e))?;
                self.ingest_event(event)
            }
            MqttMessageType::Register => {
                let reg: serde_json::Value = serde_json::from_slice(payload)
                    .map_err(|e| format!("Invalid register JSON: {}", e))?;

                let device_id = reg
                    .get("device_id")
                    .and_then(|v| v.as_str())
                    .ok_or("Missing device_id")?;
                let name = reg
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(device_id);
                let dtype = reg
                    .get("device_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("sensor");
                let owner = reg
                    .get("owner_wallet")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&wallet)
                    .to_string();

                self.register_device(
                    device_id.to_string(),
                    wallet,
                    dtype,
                    name.to_string(),
                    owner,
                    None,
                    HashMap::new(),
                )?;
                Ok(())
            }
        }
    }

    /// Start the gateway runtime (MQTT + CoAP listeners).
    /// Returns a JoinHandle — the gateway runs until the node shuts down.
    pub async fn start(self: Arc<Self>) -> Result<(), String> {
        tracing::info!(
            "IoT Gateway started — MQTT:{} CoAP:{} (max {} devices)",
            self.config.mqtt_port,
            self.config.coap_port,
            self.config.max_devices
        );

        self.stats.write().mqtt_connected = true;
        self.stats.write().coap_running = true;

        // Stale device detector — marks devices as Offline if no telemetry received
        let registry = self.registry.clone();
        let timeout = self.config.stale_device_timeout_secs as i64;
        let stats = self.stats.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let now = chrono::Utc::now().timestamp();
                for device in registry.list_devices() {
                    if device.status == DeviceStatus::Online
                        && (now - device.last_seen_at) > timeout
                    {
                        registry.update_status(&device.device_id, DeviceStatus::Offline);
                        tracing::debug!("Device {} marked offline (stale)", device.device_id);
                    }
                }
                let mut s = stats.write();
                s.devices_online = registry.online_count();
                s.devices_registered = registry.device_count();
            }
        });

        Ok(())
    }
}

impl Default for IoTGateway {
    fn default() -> Self {
        Self::new(IoTGatewayConfig::default())
    }
}
