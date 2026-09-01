//! Persistent per-address transaction / log / internal-txn index for
//! Datachain Rope.
//!
//! # Layers
//!
//! * [`schema`] - RocksDB column families, composite-key encoding, and
//!   `bincode`-serialised value payloads shared by writer and reader.
//! * [`store`] - RocksDB open / options / atomic `WriteBatch`
//!   construction. Read-write handle for the indexer service, read-only
//!   handle for DCScan.
//! * [`rpc`] - minimal JSON-RPC client used by the indexer to fetch
//!   canonical blocks + logs from Reth (BLUE / GREEN / DO-rpc-*), with
//!   per-URL failover.
//! * [`writer`] - [`writer::ingest_block`]: turn one canonical block
//!   into a single atomic RocksDB batch of per-address entries.
//! * [`reorg`] - [`reorg::unwind_block`]: on a reorg detection, delete
//!   every entry recorded under the orphaned block.
//! * [`tip`] - [`tip::follow_tip`] + [`tip::backfill_range`]: the long
//!   loops that drive the writer. Reorg-safe by construction.
//! * [`reader`] - [`reader::AddressIndex`]: what DCScan calls to answer
//!   `/api/v2/addresses/:addr/transactions?cursor=...`. Newest-first
//!   reverse iteration with opaque cursors.
//!
//! # Reorg contract (must-hold invariant)
//!
//! Every canonical block is written as one `WriteBatch` that includes
//! **all** of: the `(addr, block, idx)` entries in `addr_tx`, `addr_log`,
//! `addr_internal`, the per-block address set in `meta`, and the
//! canonical block hash in `meta`. Because RocksDB `WriteBatch::commit`
//! is atomic and fsync'd, the reader can never observe a partial block.
//! Reorg unwind uses the same batch shape in reverse.

pub mod reader;
pub mod reorg;
pub mod rpc;
pub mod schema;
pub mod store;
pub mod tip;
pub mod writer;

pub use rpc::RpcClient;
pub use schema::{Cursor, LogRef, LogRole, TxRef, TxRole};
pub use store::{Store, StoreError};
