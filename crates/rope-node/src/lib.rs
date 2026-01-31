//! # Datachain Rope Node
//! 
//! Full node implementation for the Datachain Rope network.

pub mod config;
pub mod node;
pub mod genesis;
pub mod rpc_server;
pub mod metrics;
pub mod string_producer;

pub use config::NodeConfig;
pub use node::RopeNode;
pub use string_producer::{StringProducer, StringProducerConfig, ProductionEvent, ProductionStats};
