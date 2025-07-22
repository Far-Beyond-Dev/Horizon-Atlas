use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
use ring::rand::{SecureRandom, SystemRandom};
use ring::{aead, pbkdf2};
use std::num::NonZeroU32;
use std::time::Instant;
use tracing::{info, warn, error, debug};

const CREDENTIAL_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;
const TAG_LEN: usize = 16;

pub struct EncryptionManager {
    enabled: bool,
    key: [u8; CREDENTIAL_LEN],
    rng: SystemRandom,
}

impl EncryptionManager {
    pub fn new(enabled: bool, password: &str) -> Result<Self> {
        info!("Initializing encryption manager (enabled: {})", enabled);
        
        if !enabled {
            info!("Encryption disabled - data will be transmitted in plaintext");
            return Ok(Self {
                enabled,
                key: [0u8; CREDENTIAL_LEN], // Dummy key when disabled
                rng: SystemRandom::new(),
            });
        }
        
        debug!("Starting key derivation process...");
        let start_time = Instant::now();
        
        let mut key = [0u8; CREDENTIAL_LEN];
        let salt = b"horizon_atlas_salt_2024";
        
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(100_000).unwrap(),
            salt,
            password.as_bytes(),
            &mut key,
        );
        
        let key_derivation_time = start_time.elapsed();
        info!("Key derivation completed in {:?}", key_derivation_time);
        
        if key_derivation_time.as_millis() > 1000 {
            warn!("Key derivation took longer than expected: {:?}", key_derivation_time);
        }
        
        info!("Encryption manager initialized successfully with AES-256-GCM");
        
        Ok(Self {
            enabled,
            key,
            rng: SystemRandom::new(),
        })
    }
    
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        let start_time = Instant::now();
        let data_size = data.len();
        
        debug!("Starting encryption of {} bytes", data_size);
        
        if !self.enabled {
            debug!("Encryption bypassed (disabled)");
            return Ok(data.to_vec());
        }
        
        // Security audit log
        info!("Encryption operation initiated for {} bytes of data", data_size);
        
        let mut nonce = [0u8; NONCE_LEN];
        self.rng.fill(&mut nonce).map_err(|e| {
            error!("Failed to generate cryptographic nonce: {:?}", e);
            anyhow!("Failed to generate nonce: {:?}", e)
        })?;
        
        let sealing_key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.key)
                .map_err(|e| {
                    error!("Failed to create encryption key: {:?}", e);
                    anyhow!("Failed to create key: {:?}", e)
                })?
        );
        
        let mut in_out = data.to_vec();
        let tag = sealing_key.seal_in_place_separate_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut in_out,
        ).map_err(|e| {
            error!("Encryption operation failed for {} bytes: {:?}", data_size, e);
            anyhow!("Encryption failed: {:?}", e)
        })?;
        
        let mut result = Vec::with_capacity(NONCE_LEN + in_out.len() + TAG_LEN);
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&in_out);
        result.extend_from_slice(tag.as_ref());
        
        let encryption_time = start_time.elapsed();
        let encrypted_size = result.len();
        let overhead = encrypted_size.saturating_sub(data_size);
        
        info!(
            "Encryption completed: {} bytes -> {} bytes (+{} overhead) in {:?}", 
            data_size, encrypted_size, overhead, encryption_time
        );
        
        if encryption_time.as_millis() > 10 {
            warn!("Encryption took longer than expected: {:?} for {} bytes", encryption_time, data_size);
        }
        
        debug!("Encryption operation completed successfully");
        
        Ok(result)
    }
    
    pub fn decrypt(&self, encrypted_data: &[u8]) -> Result<Vec<u8>> {
        let start_time = Instant::now();
        let encrypted_size = encrypted_data.len();
        
        debug!("Starting decryption of {} bytes", encrypted_size);
        
        if !self.enabled {
            debug!("Decryption bypassed (disabled)");
            return Ok(encrypted_data.to_vec());
        }
        
        // Security audit log
        info!("Decryption operation initiated for {} bytes of encrypted data", encrypted_size);
        
        if encrypted_data.len() < NONCE_LEN + TAG_LEN {
            error!("Decryption failed: encrypted data too short ({} bytes, minimum {})", 
                   encrypted_size, NONCE_LEN + TAG_LEN);
            return Err(anyhow!("Encrypted data too short"));
        }
        
        let nonce = &encrypted_data[..NONCE_LEN];
        let ciphertext_and_tag = &encrypted_data[NONCE_LEN..];
        
        let opening_key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.key)
                .map_err(|e| {
                    error!("Failed to create decryption key: {:?}", e);
                    anyhow!("Failed to create key: {:?}", e)
                })?
        );
        
        let mut in_out = ciphertext_and_tag.to_vec();
        let plaintext = opening_key.open_in_place(
            aead::Nonce::try_assume_unique_for_key(nonce)
                .map_err(|e| {
                    error!("Invalid nonce in encrypted data: {:?}", e);
                    anyhow!("Invalid nonce: {:?}", e)
                })?,
            aead::Aad::empty(),
            &mut in_out,
        ).map_err(|e| {
            error!("Decryption failed for {} bytes - possible data corruption or tampering: {:?}", 
                   encrypted_size, e);
            anyhow!("Decryption failed: {:?}", e)
        })?;
        
        let decryption_time = start_time.elapsed();
        let decrypted_size = plaintext.len();
        let overhead = encrypted_size.saturating_sub(decrypted_size);
        
        info!(
            "Decryption completed: {} bytes -> {} bytes (-{} overhead) in {:?}", 
            encrypted_size, decrypted_size, overhead, decryption_time
        );
        
        if decryption_time.as_millis() > 10 {
            warn!("Decryption took longer than expected: {:?} for {} bytes", decryption_time, encrypted_size);
        }
        
        debug!("Decryption operation completed successfully");
        
        Ok(plaintext.to_vec())
    }
    
    pub fn encrypt_string(&self, data: &str) -> Result<String> {
        debug!("Encrypting string of {} characters", data.len());
        info!("String encryption operation initiated");
        
        let encrypted = self.encrypt(data.as_bytes())?;
        let base64_result = general_purpose::STANDARD.encode(encrypted);
        
        debug!("String encryption completed: {} chars -> {} base64 chars", data.len(), base64_result.len());
        info!("String encryption operation completed successfully");
        
        Ok(base64_result)
    }
    
    pub fn decrypt_string(&self, encrypted_data: &str) -> Result<String> {
        debug!("Decrypting base64 string of {} characters", encrypted_data.len());
        info!("String decryption operation initiated");
        
        let decoded = general_purpose::STANDARD.decode(encrypted_data)
            .map_err(|e| {
                error!("Base64 decode failed - possible data corruption: {}", e);
                anyhow!("Base64 decode error: {}", e)
            })?;
            
        let decrypted = self.decrypt(&decoded)?;
        
        let result_string = String::from_utf8(decrypted)
            .map_err(|e| {
                error!("UTF-8 decode failed - possible data corruption or encoding issue: {}", e);
                anyhow!("UTF-8 decode error: {}", e)
            })?;
            
        debug!("String decryption completed: {} base64 chars -> {} chars", encrypted_data.len(), result_string.len());
        info!("String decryption operation completed successfully");
        
        Ok(result_string)
    }
}