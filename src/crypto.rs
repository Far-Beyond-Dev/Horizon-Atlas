use crate::config::{CryptoConfig, CipherSuite};
use crate::errors::{AtlasError, Result};
use parking_lot::RwLock;
use ring::{aead, pbkdf2, rand::{SecureRandom, SystemRandom}};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::interval;
use tracing::{debug, info, error, warn};

const CHACHA20_POLY1305_KEY_LEN: usize = 32;
const AES_256_GCM_KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 32;

pub struct CryptoManager {
    config: CryptoConfig,
    current_key: Arc<RwLock<EncryptionKey>>,
    key_history: Arc<RwLock<HashMap<u32, EncryptionKey>>>,
    rng: SystemRandom,
    key_rotation_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
struct EncryptionKey {
    id: u32,
    key: Vec<u8>,
    algorithm: aead::Algorithm,
    created_at: SystemTime,
    expires_at: SystemTime,
}

impl EncryptionKey {
    fn new(id: u32, key: Vec<u8>, cipher_suite: &CipherSuite, ttl: Duration) -> Self {
        let algorithm = match cipher_suite {
            CipherSuite::ChaCha20Poly1305 => aead::CHACHA20_POLY1305,
            CipherSuite::Aes256Gcm => aead::AES_256_GCM,
        };
        
        let now = SystemTime::now();
        
        Self {
            id,
            key,
            algorithm,
            created_at: now,
            expires_at: now + ttl,
        }
    }
    
    fn is_expired(&self) -> bool {
        SystemTime::now() > self.expires_at
    }
}

pub struct EncryptedData {
    pub key_id: u32,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

impl CryptoManager {
    pub fn new(config: CryptoConfig) -> Result<Self> {
        if !config.enabled {
            return Ok(Self {
                config,
                current_key: Arc::new(RwLock::new(Self::create_dummy_key())),
                key_history: Arc::new(RwLock::new(HashMap::new())),
                rng: SystemRandom::new(),
                key_rotation_task: None,
            });
        }
        
        let initial_key = Self::generate_key(1, &config)?;
        let mut key_history = HashMap::new();
        key_history.insert(initial_key.id, initial_key.clone());
        
        Ok(Self {
            config,
            current_key: Arc::new(RwLock::new(initial_key)),
            key_history: Arc::new(RwLock::new(key_history)),
            rng: SystemRandom::new(),
            key_rotation_task: None,
        })
    }
    
    pub async fn start(&mut self) -> Result<()> {
        if !self.config.enabled {
            info!("Cryptographic operations disabled");
            return Ok(());
        }
        
        info!("Starting crypto manager with {:?} cipher suite", self.config.cipher_suite);
        
        let config = self.config.clone();
        let current_key = Arc::clone(&self.current_key);
        let key_history = Arc::clone(&self.key_history);
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(config.key_rotation_interval / 4));
            let mut next_key_id = 2u32;
            
            loop {
                interval.tick().await;
                
                let should_rotate = {
                    let key = current_key.read();
                    let time_until_expiry = key.expires_at
                        .duration_since(SystemTime::now())
                        .unwrap_or(Duration::ZERO);
                    
                    time_until_expiry < Duration::from_millis(config.key_rotation_interval / 4)
                };
                
                if should_rotate {
                    match Self::generate_key(next_key_id, &config) {
                        Ok(new_key) => {
                            info!("Rotating encryption key from {} to {}", 
                                  current_key.read().id, new_key.id);
                            
                            key_history.write().insert(new_key.id, new_key.clone());
                            *current_key.write() = new_key;
                            next_key_id += 1;
                            
                            Self::cleanup_expired_keys(&key_history);
                        }
                        Err(e) => {
                            error!("Failed to generate new encryption key: {}", e);
                        }
                    }
                }
            }
        });
        
        self.key_rotation_task = Some(task);
        Ok(())
    }
    
    pub async fn stop(&mut self) {
        if let Some(task) = self.key_rotation_task.take() {
            task.abort();
        }
        info!("Crypto manager stopped");
    }
    
    pub async fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if !self.config.enabled {
            return Ok(plaintext.to_vec());
        }
        
        let key = self.current_key.read().clone();
        let mut nonce = vec![0u8; NONCE_LEN];
        self.rng.fill(&mut nonce)
            .map_err(|_| AtlasError::Crypto("Failed to generate nonce".to_string()))?;
        
        let unbound_key = aead::UnboundKey::new(&key.algorithm, &key.key)
            .map_err(|_| AtlasError::Crypto("Invalid encryption key".to_string()))?;
        let sealing_key = aead::LessSafeKey::new(unbound_key);
        
        let nonce_seq = aead::Nonce::assume_unique_for_key(
            <[u8; NONCE_LEN]>::try_from(nonce.as_slice())
                .map_err(|_| AtlasError::Crypto("Invalid nonce length".to_string()))?
        );
        
        let mut in_out = plaintext.to_vec();
        sealing_key.seal_in_place_append_tag(nonce_seq, aead::Aad::empty(), &mut in_out)
            .map_err(|_| AtlasError::Crypto("Encryption failed".to_string()))?;
        
        let encrypted_data = EncryptedData {
            key_id: key.id,
            nonce,
            ciphertext: in_out,
        };
        
        self.serialize_encrypted_data(&encrypted_data)
    }
    
    pub async fn decrypt(&self, encrypted_data: &[u8]) -> Result<Vec<u8>> {
        if !self.config.enabled {
            return Ok(encrypted_data.to_vec());
        }
        
        let encrypted = self.deserialize_encrypted_data(encrypted_data)?;
        
        let key = {
            let key_history = self.key_history.read();
            key_history.get(&encrypted.key_id)
                .ok_or_else(|| AtlasError::Crypto(format!("Unknown key ID: {}", encrypted.key_id)))?
                .clone()
        };
        
        if key.is_expired() {
            return Err(AtlasError::Crypto("Key has expired".to_string()));
        }
        
        let unbound_key = aead::UnboundKey::new(&key.algorithm, &key.key)
            .map_err(|_| AtlasError::Crypto("Invalid decryption key".to_string()))?;
        let opening_key = aead::LessSafeKey::new(unbound_key);
        
        let nonce = aead::Nonce::assume_unique_for_key(
            <[u8; NONCE_LEN]>::try_from(encrypted.nonce.as_slice())
                .map_err(|_| AtlasError::Crypto("Invalid nonce length".to_string()))?
        );
        
        let mut ciphertext = encrypted.ciphertext;
        let plaintext = opening_key.open_in_place(nonce, aead::Aad::empty(), &mut ciphertext)
            .map_err(|_| AtlasError::Crypto("Decryption failed".to_string()))?;
        
        Ok(plaintext.to_vec())
    }
    
    pub fn is_encryption_enabled(&self) -> bool {
        self.config.enabled
    }
    
    pub async fn derive_key_from_password(&self, password: &str, salt: &[u8]) -> Result<Vec<u8>> {
        let key_len = match self.config.cipher_suite {
            CipherSuite::ChaCha20Poly1305 => CHACHA20_POLY1305_KEY_LEN,
            CipherSuite::Aes256Gcm => AES_256_GCM_KEY_LEN,
        };
        
        let mut key = vec![0u8; key_len];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            std::num::NonZeroU32::new(self.config.key_derivation.iterations)
                .ok_or_else(|| AtlasError::Crypto("Invalid iteration count".to_string()))?,
            salt,
            password.as_bytes(),
            &mut key,
        );
        
        Ok(key)
    }
    
    pub fn generate_salt(&self) -> Result<Vec<u8>> {
        let mut salt = vec![0u8; SALT_LEN];
        self.rng.fill(&mut salt)
            .map_err(|_| AtlasError::Crypto("Failed to generate salt".to_string()))?;
        Ok(salt)
    }
    
    pub fn get_current_key_id(&self) -> u32 {
        self.current_key.read().id
    }
    
    pub fn get_key_info(&self) -> Vec<KeyInfo> {
        self.key_history.read().iter().map(|(id, key)| KeyInfo {
            id: *id,
            created_at: key.created_at,
            expires_at: key.expires_at,
            is_current: *id == self.current_key.read().id,
            is_expired: key.is_expired(),
        }).collect()
    }
    
    fn generate_key(id: u32, config: &CryptoConfig) -> Result<EncryptionKey> {
        let key_len = match config.cipher_suite {
            CipherSuite::ChaCha20Poly1305 => CHACHA20_POLY1305_KEY_LEN,
            CipherSuite::Aes256Gcm => AES_256_GCM_KEY_LEN,
        };
        
        let rng = SystemRandom::new();
        let mut key = vec![0u8; key_len];
        rng.fill(&mut key)
            .map_err(|_| AtlasError::Crypto("Failed to generate key".to_string()))?;
        
        let ttl = Duration::from_millis(config.key_rotation_interval);
        Ok(EncryptionKey::new(id, key, &config.cipher_suite, ttl))
    }
    
    fn create_dummy_key() -> EncryptionKey {
        EncryptionKey::new(
            0,
            vec![0u8; CHACHA20_POLY1305_KEY_LEN],
            &CipherSuite::ChaCha20Poly1305,
            Duration::from_secs(86400),
        )
    }
    
    fn cleanup_expired_keys(key_history: &Arc<RwLock<HashMap<u32, EncryptionKey>>>) {
        let mut history = key_history.write();
        let expired_keys: Vec<u32> = history.iter()
            .filter_map(|(id, key)| {
                if key.is_expired() && 
                   SystemTime::now().duration_since(key.expires_at).unwrap_or(Duration::ZERO) > Duration::from_secs(300) {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        
        for key_id in expired_keys {
            history.remove(&key_id);
            debug!("Removed expired key {}", key_id);
        }
    }
    
    fn serialize_encrypted_data(&self, data: &EncryptedData) -> Result<Vec<u8>> {
        bincode::serialize(data)
            .map_err(|e| AtlasError::Crypto(format!("Failed to serialize encrypted data: {}", e)))
    }
    
    fn deserialize_encrypted_data(&self, data: &[u8]) -> Result<EncryptedData> {
        bincode::deserialize(data)
            .map_err(|e| AtlasError::Crypto(format!("Failed to deserialize encrypted data: {}", e)))
    }
}

impl serde::Serialize for EncryptedData {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("EncryptedData", 3)?;
        state.serialize_field("key_id", &self.key_id)?;
        state.serialize_field("nonce", &self.nonce)?;
        state.serialize_field("ciphertext", &self.ciphertext)?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for EncryptedData {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        #[derive(serde::Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field { KeyId, Nonce, Ciphertext }

        struct EncryptedDataVisitor;

        impl<'de> Visitor<'de> for EncryptedDataVisitor {
            type Value = EncryptedData;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct EncryptedData")
            }

            fn visit_map<V>(self, mut map: V) -> std::result::Result<EncryptedData, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut key_id = None;
                let mut nonce = None;
                let mut ciphertext = None;
                
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::KeyId => {
                            if key_id.is_some() {
                                return Err(de::Error::duplicate_field("key_id"));
                            }
                            key_id = Some(map.next_value()?);
                        }
                        Field::Nonce => {
                            if nonce.is_some() {
                                return Err(de::Error::duplicate_field("nonce"));
                            }
                            nonce = Some(map.next_value()?);
                        }
                        Field::Ciphertext => {
                            if ciphertext.is_some() {
                                return Err(de::Error::duplicate_field("ciphertext"));
                            }
                            ciphertext = Some(map.next_value()?);
                        }
                    }
                }
                
                let key_id = key_id.ok_or_else(|| de::Error::missing_field("key_id"))?;
                let nonce = nonce.ok_or_else(|| de::Error::missing_field("nonce"))?;
                let ciphertext = ciphertext.ok_or_else(|| de::Error::missing_field("ciphertext"))?;
                
                Ok(EncryptedData { key_id, nonce, ciphertext })
            }
        }

        const FIELDS: &[&str] = &["key_id", "nonce", "ciphertext"];
        deserializer.deserialize_struct("EncryptedData", FIELDS, EncryptedDataVisitor)
    }
}

#[derive(Debug)]
pub struct KeyInfo {
    pub id: u32,
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
    pub is_current: bool,
    pub is_expired: bool,
}

pub struct SessionCrypto {
    session_keys: Arc<RwLock<HashMap<String, SessionKey>>>,
    rng: SystemRandom,
}

#[derive(Clone)]
struct SessionKey {
    key: Vec<u8>,
    algorithm: aead::Algorithm,
    created_at: SystemTime,
    last_used: SystemTime,
}

impl SessionCrypto {
    pub fn new() -> Self {
        Self {
            session_keys: Arc::new(RwLock::new(HashMap::new())),
            rng: SystemRandom::new(),
        }
    }
    
    pub async fn create_session(&self, session_id: String, cipher_suite: &CipherSuite) -> Result<()> {
        let key_len = match cipher_suite {
            CipherSuite::ChaCha20Poly1305 => CHACHA20_POLY1305_KEY_LEN,
            CipherSuite::Aes256Gcm => AES_256_GCM_KEY_LEN,
        };
        
        let algorithm = match cipher_suite {
            CipherSuite::ChaCha20Poly1305 => aead::CHACHA20_POLY1305,
            CipherSuite::Aes256Gcm => aead::AES_256_GCM,
        };
        
        let mut key = vec![0u8; key_len];
        self.rng.fill(&mut key)
            .map_err(|_| AtlasError::Crypto("Failed to generate session key".to_string()))?;
        
        let session_key = SessionKey {
            key,
            algorithm,
            created_at: SystemTime::now(),
            last_used: SystemTime::now(),
        };
        
        self.session_keys.write().insert(session_id, session_key);
        Ok(())
    }
    
    pub async fn encrypt_for_session(&self, session_id: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        let key = {
            let mut keys = self.session_keys.write();
            let session_key = keys.get_mut(session_id)
                .ok_or_else(|| AtlasError::Crypto("Session not found".to_string()))?;
            session_key.last_used = SystemTime::now();
            session_key.clone()
        };
        
        let mut nonce = vec![0u8; NONCE_LEN];
        self.rng.fill(&mut nonce)
            .map_err(|_| AtlasError::Crypto("Failed to generate nonce".to_string()))?;
        
        let unbound_key = aead::UnboundKey::new(&key.algorithm, &key.key)
            .map_err(|_| AtlasError::Crypto("Invalid session key".to_string()))?;
        let sealing_key = aead::LessSafeKey::new(unbound_key);
        
        let nonce_seq = aead::Nonce::assume_unique_for_key(
            <[u8; NONCE_LEN]>::try_from(nonce.as_slice())
                .map_err(|_| AtlasError::Crypto("Invalid nonce length".to_string()))?
        );
        
        let mut in_out = plaintext.to_vec();
        sealing_key.seal_in_place_append_tag(nonce_seq, aead::Aad::empty(), &mut in_out)
            .map_err(|_| AtlasError::Crypto("Session encryption failed".to_string()))?;
        
        let mut result = nonce;
        result.extend_from_slice(&in_out);
        Ok(result)
    }
    
    pub async fn decrypt_for_session(&self, session_id: &str, encrypted_data: &[u8]) -> Result<Vec<u8>> {
        if encrypted_data.len() < NONCE_LEN {
            return Err(AtlasError::Crypto("Invalid encrypted data length".to_string()));
        }
        
        let key = {
            let mut keys = self.session_keys.write();
            let session_key = keys.get_mut(session_id)
                .ok_or_else(|| AtlasError::Crypto("Session not found".to_string()))?;
            session_key.last_used = SystemTime::now();
            session_key.clone()
        };
        
        let (nonce, ciphertext) = encrypted_data.split_at(NONCE_LEN);
        
        let unbound_key = aead::UnboundKey::new(&key.algorithm, &key.key)
            .map_err(|_| AtlasError::Crypto("Invalid session key".to_string()))?;
        let opening_key = aead::LessSafeKey::new(unbound_key);
        
        let nonce_seq = aead::Nonce::assume_unique_for_key(
            <[u8; NONCE_LEN]>::try_from(nonce)
                .map_err(|_| AtlasError::Crypto("Invalid nonce length".to_string()))?
        );
        
        let mut ciphertext = ciphertext.to_vec();
        let plaintext = opening_key.open_in_place(nonce_seq, aead::Aad::empty(), &mut ciphertext)
            .map_err(|_| AtlasError::Crypto("Session decryption failed".to_string()))?;
        
        Ok(plaintext.to_vec())
    }
    
    pub async fn remove_session(&self, session_id: &str) {
        self.session_keys.write().remove(session_id);
    }
    
    pub async fn cleanup_expired_sessions(&self, max_age: Duration) {
        let now = SystemTime::now();
        let mut keys = self.session_keys.write();
        
        keys.retain(|_, session_key| {
            now.duration_since(session_key.last_used).unwrap_or(Duration::ZERO) < max_age
        });
    }
}