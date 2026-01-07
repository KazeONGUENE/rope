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

pub mod testimony_agent;
pub mod tool_registry;
pub mod invocation_engine;
pub mod security_policy;
pub mod protocol_adapters;
pub mod digital_credits;
pub mod governance;

// Re-exports
pub use testimony_agent::*;
pub use tool_registry::*;
pub use invocation_engine::*;
pub use security_policy::*;
pub use digital_credits::*;
pub use governance::*;

