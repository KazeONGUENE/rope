//! Scenarios — each subcommand implementation lives here.
//!
//! The split into one module per scenario keeps `main.rs` thin and
//! each scenario individually testable.

pub mod store_mixed;
pub mod store_recover;
pub mod store_write;
