//! # Smartchain - The Intelligent Information Network
//!
//! **Datachain Rope is NOT a blockchain. It is a Smartchain.**
//!
//! A Smartchain is an intelligent distributed system that:
//! - Uses AI agents as "Testimonies" to secure and validate data
//! - Can invoke vetted external tools to execute transactions
//! - Adapts its security model based on context
//! - Connects to any protocol: blockchain, banking, finance, asset management
//!
//! ## Network Information
//!
//! | Parameter | Value |
//! |-----------|-------|
//! | **Network Name** | Datachain Rope |
//! | **Chain ID** | 271828 (0x425D4) |
//! | **Currency Symbol** | FAT |
//! | **Currency Name** | DC FAT |
//! | **Decimals** | 18 |
//! | **RPC URL** | <https://erpc.datachain.network> |
//! | **RPC URL (Alt)** | <https://erpc.rope.network> |
//! | **WebSocket** | <wss://ws.datachain.network> |
//! | **Block Explorer** | <https://dcscan.io> |
//! | **Primary Domain** | datachain.network |
//! | **Secondary Domain** | rope.network |
//!
//! ### Bridge Contracts
//!
//! | Bridge | Contract Address |
//! |--------|-----------------|
//! | Ethereum | 0x0b44547be0a0df5dcd5327de8ea73680517c5a54 |
//! | XDC | 0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a |
//!
//! ## Core Concepts
//!
//! ### AI Testimonies
//!
//! Traditional blockchains rely on cryptographic proofs and consensus mechanisms
//! that are purely mathematical. Smartchain extends this with AI Testimonies:
//!
//! - **Testimony Agents**: Specialized AI models that validate transactions
//! - **Contextual Validation**: Understand the semantic meaning of data
//! - **Adaptive Security**: Adjust validation requirements based on risk
//! - **Anomaly Detection**: Identify suspicious patterns in real-time
//!
//! ### Vetted Tool Registry
//!
//! The Smartchain can invoke external tools to execute transactions:
//!
//! - **Blockchain Protocols**: Ethereum, Polkadot, XDC, Bitcoin, etc.
//! - **Banking Protocols**: SWIFT, SEPA, ACH, FedWire
//! - **Finance Protocols**: FIX, Bloomberg, Refinitiv
//! - **Asset Management**: Custody solutions, trading platforms
//! - **Custom Protocols**: Any vetted external service
//!
//! Tools must be:
//! 1. Registered in the VettedToolRegistry
//! 2. Audited and approved by governance
//! 3. Continuously monitored for security
//!
//! ## Architecture
//!
//! ```text
//!                      ┌──────────────────────────────────┐
//!                      │         SMARTCHAIN CORE          │
//!                      │                                  │
//!   String Lattice ────┤  AI Testimony   Tool Invocation  │
//!                      │     Engine          Engine       │
//!                      │        │                │        │
//!                      └────────┼────────────────┼────────┘
//!                               │                │
//!           ┌──────────────────┬┴────────────────┼──────────────────┐
//!           │                  │                 │                  │
//!           ▼                  ▼                 ▼                  ▼
//!     ┌──────────┐      ┌──────────┐      ┌──────────┐      ┌──────────┐
//!     │ Ethereum │      │  Banking │      │ Finance  │      │  Asset   │
//!     │  Bridge  │      │ Protocol │      │ Protocol │      │ Mgmt API │
//!     └──────────┘      └──────────┘      └──────────┘      └──────────┘
//! ```

pub mod digital_credits;
pub mod governance;
pub mod invocation_engine;
pub mod network_config;
pub mod protocol_adapters;
pub mod security_policy;
pub mod testimony_agent;
pub mod testimony_policy;
pub mod tool_registry;

// Re-exports
pub use digital_credits::*;
pub use governance::*;
pub use invocation_engine::*;
pub use network_config::*;
pub use security_policy::*;
pub use testimony_agent::*;
pub use testimony_policy::*;
pub use tool_registry::*;
