//! Error type for the shadow witness.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ShadowWitnessError {
    #[error("config error: {0}")]
    Config(String),

    #[error("rocksdb error: {0}")]
    Rocks(#[from] rocksdb::Error),

    #[error("rpc transport error: {0}")]
    Rpc(#[from] reqwest::Error),

    #[error("rpc returned error: code {code}, message: {message}")]
    RpcRemote { code: i64, message: String },

    #[error("rpc payload was not valid JSON-RPC: {0}")]
    RpcDecode(String),

    #[error("serialisation error: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("hex decode error: {0}")]
    Hex(#[from] hex::FromHexError),

    #[error("server bind error: {0}")]
    Bind(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("internal invariant violated: {0}")]
    Internal(String),
}

pub type ShadowWitnessResult<T> = Result<T, ShadowWitnessError>;
