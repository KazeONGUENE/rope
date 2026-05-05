//! # Rope IoT Gateway
//!
//! Native IoT protocol bridge for Datachain Rope. Accepts telemetry from
//! devices over MQTT and CoAP, and writes each reading as an encrypted
//! fragment on the device's personal String (ledger).
//!
//! ## Supported Protocols
//!
//! | Protocol | Port | Use Case |
//! |----------|------|----------|
//! | MQTT 3.1.1 | 1883 (TCP) | Sensors, actuators, gateways |
//! | CoAP | 5683 (UDP) | Constrained devices, battery-powered |
//! | HTTP POST | (via RPC) | Cloud-to-Rope bridge |
//!
//! ## Topic Convention (MQTT)
//!
//! ```text
//! rope/{device_wallet_address}/telemetry   → sensor readings
//! rope/{device_wallet_address}/event        → discrete events (door open, alarm)
//! rope/{device_wallet_address}/diagnostic   → self-diagnosis reports
//! rope/{device_wallet_address}/command      → commands TO the device (subscribe)
//! ```
//!
//! ## Device Registry
//!
//! Each IoT device is identified by a wallet address. The gateway maintains
//! a registry mapping device IDs to wallet addresses and metadata (type,
//! location, firmware version, owner). Registration happens via RPC
//! (`rope_registerDevice`) or MQTT (`rope/register` topic).
//!
//! ## Architecture
//!
//! ```text
//! IoT Device ──MQTT──► IoT Gateway ──► LedgerManager.append_to_ledger()
//!                                          │
//! IoT Device ──CoAP──► IoT Gateway ────────┘
//! ```

pub mod device;
pub mod gateway;
pub mod protocol;

pub use device::{DeviceInfo, DeviceRegistry, DeviceStatus, DeviceType};
pub use gateway::{IoTGateway, IoTGatewayConfig, GatewayStats};
pub use protocol::{TelemetryPayload, TelemetryBatch, DeviceEvent};
