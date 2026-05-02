//! Protocol-agnostic telemetry types — the common data model that MQTT,
//! CoAP, and HTTP payloads are normalized into before writing to a String.

use hashbrown::HashMap;
use serde::{Deserialize, Serialize};

/// A single telemetry reading from an IoT device.
///
/// This is the canonical format that the gateway normalizes all incoming
/// protocol-specific payloads into before appending to the device's personal String.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemetryPayload {
    pub device_wallet: String,
    pub timestamp: i64,
    pub readings: HashMap<String, TelemetryValue>,
    pub source_protocol: SourceProtocol,
    pub sequence_number: Option<u64>,
    pub quality: Option<DataQuality>,
}

/// Typed telemetry values — supports numeric, boolean, string, and geo.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TelemetryValue {
    Float(f64),
    Integer(i64),
    Boolean(bool),
    Text(String),
    Geo { lat: f64, lng: f64 },
    Binary(Vec<u8>),
}

impl std::fmt::Display for TelemetryValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Float(v) => write!(f, "{}", v),
            Self::Integer(v) => write!(f, "{}", v),
            Self::Boolean(v) => write!(f, "{}", v),
            Self::Text(v) => write!(f, "{}", v),
            Self::Geo { lat, lng } => write!(f, "{},{}", lat, lng),
            Self::Binary(v) => write!(f, "<{}bytes>", v.len()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceProtocol {
    Mqtt,
    Coap,
    Http,
    Internal,
}

impl SourceProtocol {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Mqtt => "mqtt",
            Self::Coap => "coap",
            Self::Http => "http",
            Self::Internal => "internal",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DataQuality {
    Good,
    Uncertain,
    Bad,
    OutOfRange,
}

/// A batch of telemetry readings (for devices that buffer and send periodically).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelemetryBatch {
    pub device_wallet: String,
    pub payloads: Vec<TelemetryPayload>,
    pub batch_timestamp: i64,
}

/// Discrete device event (as opposed to continuous telemetry).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceEvent {
    pub device_wallet: String,
    pub event_type: DeviceEventType,
    pub severity: EventSeverity,
    pub description: String,
    pub timestamp: i64,
    pub metadata: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceEventType {
    Alarm,
    StateChange,
    ThresholdBreach,
    Heartbeat,
    FirmwareUpdate,
    Reboot,
    Error,
    Diagnostic,
    Custom(String),
}

impl DeviceEventType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Alarm => "alarm",
            Self::StateChange => "state_change",
            Self::ThresholdBreach => "threshold_breach",
            Self::Heartbeat => "heartbeat",
            Self::FirmwareUpdate => "firmware_update",
            Self::Reboot => "reboot",
            Self::Error => "error",
            Self::Diagnostic => "diagnostic",
            Self::Custom(s) => s,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventSeverity {
    Info,
    Warning,
    Critical,
    Emergency,
}

/// Normalize an MQTT topic + payload into the canonical format.
///
/// Topic format: `rope/{wallet_address}/telemetry` or `rope/{wallet_address}/event`
pub fn parse_mqtt_topic(topic: &str) -> Option<(String, MqttMessageType)> {
    let parts: Vec<&str> = topic.split('/').collect();
    if parts.len() < 3 || parts[0] != "rope" {
        return None;
    }
    let wallet = parts[1].to_string();
    let msg_type = match parts[2] {
        "telemetry" => MqttMessageType::Telemetry,
        "event" => MqttMessageType::Event,
        "diagnostic" => MqttMessageType::Diagnostic,
        "register" => MqttMessageType::Register,
        _ => return None,
    };
    Some((wallet, msg_type))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MqttMessageType {
    Telemetry,
    Event,
    Diagnostic,
    Register,
}
