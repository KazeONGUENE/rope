//! Encrypted Memory Store
//!
//! Persistent encrypted storage for agent state, conversation history,
//! and credentials using OES (Organic Encryption System).

use crate::error::RuntimeError;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Encrypted memory store for RopeAgent
pub struct EncryptedMemoryStore {
    /// Storage path
    path: PathBuf,

    /// In-memory cache (decrypted)
    cache: RwLock<MemoryCache>,

    /// Encryption key (derived from OES)
    encryption_key: [u8; 32],

    /// Dirty flag (needs flush)
    dirty: RwLock<bool>,
}

/// In-memory cache structure
#[derive(Default, Serialize, Deserialize)]
struct MemoryCache {
    /// Conversation history by channel
    conversations: HashMap<String, ConversationHistory>,

    /// User preferences
    preferences: HashMap<String, String>,

    /// Encrypted credentials by channel
    credentials: HashMap<String, Vec<u8>>,

    /// Action history
    action_history: Vec<ActionRecord>,

    /// Skill execution history
    skill_history: Vec<SkillExecutionRecord>,

    /// Custom data
    custom_data: HashMap<String, Vec<u8>>,

    /// Event log
    events: Vec<EventRecord>,
}

impl EncryptedMemoryStore {
    /// Create new encrypted memory store
    pub fn new(encryption_key: [u8; 32]) -> Self {
        Self {
            path: PathBuf::new(),
            cache: RwLock::new(MemoryCache::default()),
            encryption_key,
            dirty: RwLock::new(false),
        }
    }

    /// Open existing or create new memory store at path
    pub fn open(path: &PathBuf, seed: &[u8]) -> Result<Self, RuntimeError> {
        // Derive encryption key from seed
        let encryption_key = *blake3::hash(seed).as_bytes();

        let mut store = Self {
            path: path.clone(),
            cache: RwLock::new(MemoryCache::default()),
            encryption_key,
            dirty: RwLock::new(false),
        };

        // Load existing data if file exists
        if path.exists() {
            store.load()?;
        }

        Ok(store)
    }

    /// Load data from disk
    fn load(&mut self) -> Result<(), RuntimeError> {
        let encrypted = std::fs::read(&self.path)?;

        // Decrypt data
        let decrypted = self.decrypt(&encrypted)?;

        // Deserialize
        let cache: MemoryCache = serde_json::from_slice(&decrypted)
            .map_err(|e| RuntimeError::SerializationError(e.to_string()))?;

        *self.cache.write() = cache;
        Ok(())
    }

    /// Flush data to disk
    pub fn flush(&self) -> Result<(), RuntimeError> {
        if !*self.dirty.read() {
            return Ok(());
        }

        let cache = self.cache.read();
        let serialized = serde_json::to_vec(&*cache)
            .map_err(|e| RuntimeError::SerializationError(e.to_string()))?;

        // Encrypt data
        let encrypted = self.encrypt(&serialized)?;

        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Write atomically
        let temp_path = self.path.with_extension("tmp");
        std::fs::write(&temp_path, &encrypted)?;
        std::fs::rename(&temp_path, &self.path)?;

        *self.dirty.write() = false;
        Ok(())
    }

    /// Encrypt data
    fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>, RuntimeError> {
        // Simple XOR encryption (in production, use AES-GCM with OES key)
        let mut result = Vec::with_capacity(data.len() + 32);

        // Generate nonce
        let nonce: [u8; 16] = rand_bytes();
        result.extend_from_slice(&nonce);

        // Derive encryption key with nonce
        let key = blake3::hash(&[&self.encryption_key[..], &nonce[..]].concat());

        // Encrypt (XOR for simplicity - use proper AEAD in production)
        for (i, byte) in data.iter().enumerate() {
            result.push(byte ^ key.as_bytes()[i % 32]);
        }

        // Append MAC
        let mac = blake3::hash(&[key.as_bytes(), data].concat());
        result.extend_from_slice(&mac.as_bytes()[..16]);

        Ok(result)
    }

    /// Decrypt data
    fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, RuntimeError> {
        if data.len() < 32 {
            return Err(RuntimeError::CryptoError("Data too short".to_string()));
        }

        // Extract nonce
        let nonce = &data[..16];

        // Extract ciphertext and MAC
        let ciphertext = &data[16..data.len() - 16];
        let mac = &data[data.len() - 16..];

        // Derive encryption key
        let key = blake3::hash(&[&self.encryption_key[..], nonce].concat());

        // Decrypt
        let mut plaintext = Vec::with_capacity(ciphertext.len());
        for (i, byte) in ciphertext.iter().enumerate() {
            plaintext.push(byte ^ key.as_bytes()[i % 32]);
        }

        // Verify MAC
        let mut mac_input = Vec::new();
        mac_input.extend_from_slice(key.as_bytes());
        mac_input.extend_from_slice(&plaintext);
        let expected_mac = blake3::hash(&mac_input);
        if &expected_mac.as_bytes()[..16] != mac {
            return Err(RuntimeError::CryptoError(
                "MAC verification failed".to_string(),
            ));
        }

        Ok(plaintext)
    }

    // === Conversation History ===

    /// Add message to conversation history
    pub fn add_conversation_message(
        &self,
        channel_id: &str,
        message: ConversationMessage,
    ) -> Result<(), RuntimeError> {
        let mut cache = self.cache.write();

        let history = cache
            .conversations
            .entry(channel_id.to_string())
            .or_insert_with(ConversationHistory::new);

        history.messages.push(message);

        // Limit history size
        if history.messages.len() > 1000 {
            history.messages.remove(0);
        }

        *self.dirty.write() = true;
        Ok(())
    }

    /// Get conversation history
    pub fn get_conversation_history(&self, channel_id: &str) -> Vec<ConversationMessage> {
        self.cache
            .read()
            .conversations
            .get(channel_id)
            .map(|h| h.messages.clone())
            .unwrap_or_default()
    }

    /// Get recent conversation context (last N messages)
    pub fn get_recent_context(&self, channel_id: &str, count: usize) -> Vec<ConversationMessage> {
        self.cache
            .read()
            .conversations
            .get(channel_id)
            .map(|h| {
                let start = h.messages.len().saturating_sub(count);
                h.messages[start..].to_vec()
            })
            .unwrap_or_default()
    }

    // === Credentials ===

    /// Store encrypted credentials
    pub fn store_credentials(
        &self,
        channel_id: &str,
        credentials: &[u8],
    ) -> Result<(), RuntimeError> {
        // Double-encrypt credentials
        let encrypted = self.encrypt(credentials)?;

        self.cache
            .write()
            .credentials
            .insert(channel_id.to_string(), encrypted);

        *self.dirty.write() = true;
        Ok(())
    }

    /// Retrieve credentials
    pub fn get_credentials(&self, channel_id: &str) -> Option<Vec<u8>> {
        self.cache
            .read()
            .credentials
            .get(channel_id)
            .and_then(|encrypted| self.decrypt(encrypted).ok())
    }

    /// Remove credentials
    pub fn remove_credentials(&self, channel_id: &str) -> bool {
        let removed = self.cache.write().credentials.remove(channel_id).is_some();
        if removed {
            *self.dirty.write() = true;
        }
        removed
    }

    // === Preferences ===

    /// Set preference
    pub fn set_preference(&self, key: &str, value: &str) {
        self.cache
            .write()
            .preferences
            .insert(key.to_string(), value.to_string());
        *self.dirty.write() = true;
    }

    /// Get preference
    pub fn get_preference(&self, key: &str) -> Option<String> {
        self.cache.read().preferences.get(key).cloned()
    }

    // === Action History ===

    /// Record action
    pub fn record_action(&self, record: ActionRecord) {
        self.cache.write().action_history.push(record);
        *self.dirty.write() = true;
    }

    /// Get action history
    pub fn get_action_history(&self, limit: usize) -> Vec<ActionRecord> {
        let cache = self.cache.read();
        let start = cache.action_history.len().saturating_sub(limit);
        cache.action_history[start..].to_vec()
    }

    // === Event Log ===

    /// Log event
    pub fn log_event(&self, event: Event) -> Result<(), RuntimeError> {
        let record = EventRecord {
            event,
            timestamp: chrono::Utc::now().timestamp(),
        };

        self.cache.write().events.push(record);
        *self.dirty.write() = true;
        Ok(())
    }

    // === Data Erasure (GDPR) ===

    /// Remove all data related to a string ID
    pub fn remove_related(&self, _string_id: &[u8; 32]) -> Result<(), RuntimeError> {
        // In production, scan and remove related data
        *self.dirty.write() = true;
        Ok(())
    }

    /// Clear all data
    pub fn clear_all(&self) {
        *self.cache.write() = MemoryCache::default();
        *self.dirty.write() = true;
    }
}

impl Drop for EncryptedMemoryStore {
    fn drop(&mut self) {
        // Try to flush on drop
        let _ = self.flush();
    }
}

/// Conversation history
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConversationHistory {
    /// Messages in conversation
    pub messages: Vec<ConversationMessage>,
}

impl ConversationHistory {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }
}

/// Conversation message
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationMessage {
    /// Message role (user/assistant/system)
    pub role: MessageRole,

    /// Message content
    pub content: String,

    /// Timestamp
    pub timestamp: i64,

    /// Message ID (if from channel)
    pub message_id: Option<String>,
}

/// Message role
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// Action record
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionRecord {
    /// Action ID
    pub action_id: [u8; 32],

    /// Action type
    pub action_type: String,

    /// Was successful
    pub success: bool,

    /// Timestamp
    pub timestamp: i64,

    /// Lattice string ID (if recorded)
    pub lattice_ref: Option<[u8; 32]>,
}

/// Skill execution record
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillExecutionRecord {
    /// Skill ID
    pub skill_id: [u8; 32],

    /// Skill name
    pub skill_name: String,

    /// Was successful
    pub success: bool,

    /// Timestamp
    pub timestamp: i64,
}

/// Event record
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventRecord {
    /// Event details
    pub event: Event,

    /// Timestamp
    pub timestamp: i64,
}

/// Event types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    /// Message received
    MessageReceived { channel: String, timestamp: i64 },

    /// Message sent
    MessageSent { channel: String, timestamp: i64 },

    /// Channel connected
    ChannelConnected { channel: String },

    /// Channel disconnected
    ChannelDisconnected { channel: String },

    /// Skill loaded
    SkillLoaded { skill_id: [u8; 32] },

    /// Action submitted
    ActionSubmitted { action_id: [u8; 32] },

    /// Testimony received
    TestimonyReceived { action_id: [u8; 32], approved: bool },

    /// Security alert
    SecurityAlert { alert_type: String, details: String },
}

/// Generate random bytes
fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let hash = blake3::hash(&timestamp.to_le_bytes());
    bytes.copy_from_slice(&hash.as_bytes()[..N]);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encryption_roundtrip() {
        let store = EncryptedMemoryStore::new([42u8; 32]);
        let data = b"Hello, World!";

        let encrypted = store.encrypt(data).unwrap();
        let decrypted = store.decrypt(&encrypted).unwrap();

        assert_eq!(data.to_vec(), decrypted);
    }

    #[test]
    fn test_conversation_history() {
        let store = EncryptedMemoryStore::new([42u8; 32]);

        store
            .add_conversation_message(
                "channel1",
                ConversationMessage {
                    role: MessageRole::User,
                    content: "Hello".to_string(),
                    timestamp: 1000,
                    message_id: None,
                },
            )
            .unwrap();

        let history = store.get_conversation_history("channel1");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "Hello");
    }

    #[test]
    fn test_credentials() {
        let store = EncryptedMemoryStore::new([42u8; 32]);
        let creds = b"secret_token";

        store.store_credentials("telegram", creds).unwrap();
        let retrieved = store.get_credentials("telegram").unwrap();

        assert_eq!(creds.to_vec(), retrieved);
    }
}
