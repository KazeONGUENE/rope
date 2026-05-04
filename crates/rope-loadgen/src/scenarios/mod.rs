//! Scenarios — each subcommand implementation lives here.
//!
//! The split into one module per scenario keeps `main.rs` thin and
//! each scenario individually testable.

pub mod cluster_write;
pub mod dag_write;
pub mod manager_write;
pub mod store_mixed;
pub mod store_recover;
pub mod store_write;
pub mod verify_batch;
