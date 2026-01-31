//! Error types for RopeAgent Runtime

use thiserror::Error;

/// Runtime errors
#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Identity error: {0}")]
    IdentityError(String),

    #[error("Channel error: {0}")]
    ChannelError(#[from] ChannelError),

    #[error("Lattice error: {0}")]
    LatticeError(String),

    #[error("Skill error: {0}")]
    SkillError(#[from] SkillError),

    #[error("Memory error: {0}")]
    MemoryError(String),

    #[error("Testimony error: {0}")]
    TestimonyError(String),

    #[error("Authorization error: {0}")]
    AuthorizationError(#[from] AuthError),

    #[error("Execution error: {0}")]
    ExecutionError(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Crypto error: {0}")]
    CryptoError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

/// Channel connection errors
#[derive(Error, Debug)]
pub enum ChannelError {
    #[error("Invalid credentials for channel: {0}")]
    InvalidCredentials(String),

    #[error("Channel not found: {0}")]
    NotFound(String),

    #[error("Channel already connected: {0}")]
    AlreadyConnected(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    #[error("Message send failed: {0}")]
    SendFailed(String),

    #[error("Unsupported channel type: {0}")]
    UnsupportedType(String),
}

/// Skill errors
#[derive(Error, Debug)]
pub enum SkillError {
    #[error("Skill not found: {0}")]
    NotFound(String),

    #[error("Skill not loaded: {0}")]
    NotLoaded(String),

    #[error("Skill not approved by governance")]
    NotApproved,

    #[error("Skill suspended: {0:?}")]
    Suspended(Option<String>),

    #[error("Missing capability: {0}")]
    MissingCapability(String),

    #[error("Validation failed: {0}")]
    ValidationFailed(String),

    #[error("Execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Invalid skill format: {0}")]
    InvalidFormat(String),
}

/// Authorization errors
#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Token not found")]
    TokenNotFound,

    #[error("Token expired")]
    TokenExpired,

    #[error("Token already used")]
    TokenAlreadyUsed,

    #[error("Action type mismatch")]
    ActionTypeMismatch,

    #[error("Value limit exceeded")]
    ValueLimitExceeded,

    #[error("OES epoch mismatch")]
    EpochMismatch,

    #[error("Invalid signature")]
    InvalidSignature,

    #[error("Insufficient permissions")]
    InsufficientPermissions,

    #[error("Identity not verified")]
    IdentityNotVerified,
}

/// Erasure errors
#[derive(Error, Debug)]
pub enum ErasureError {
    #[error("Not owner of string: {0:?}")]
    NotOwner([u8; 32]),

    #[error("String is immutable: {0:?}")]
    ImmutableString([u8; 32]),

    #[error("Erasure not confirmed")]
    NotConfirmed,

    #[error("Network error: {0}")]
    NetworkError(String),
}
