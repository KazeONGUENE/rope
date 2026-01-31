//! # Datachain Rope Node
//!
//! Full node implementation for the Datachain Rope network.

pub mod config;
pub mod genesis;
pub mod metrics;
pub mod node;
pub mod rpc_server;
pub mod string_producer;

pub use config::NodeConfig;
pub use node::RopeNode;
pub use string_producer::{ProductionEvent, ProductionStats, StringProducer, StringProducerConfig};
