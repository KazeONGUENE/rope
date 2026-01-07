//! # Datachain Rope Node
//! 
//! Complete node implementation integrating all components.
//! 
//! ## Node Types
//! 
//! - **Validator**: L0 Core Federation, consensus participation
//! - **Relay**: L1 Public, string distribution
//! - **Seeder**: RDP distribution, storage contribution

pub mod node;
pub mod config;

// Re-exports
pub use config::NodeConfig;

