use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
use ring::rand::{SecureRandom, SystemRandom};
use ring::{aead, pbkdf2};
use std::num::NonZeroU32;

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
        let mut key = [0u8; CREDENTIAL_LEN];
        let salt = b"horizon_atlas_salt_2024";
        
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(100_000).unwrap(),
            salt,
            password.as_bytes(),
            &mut key,
        );
        
        Ok(Self {
            enabled,
            key,
            rng: SystemRandom::new(),
        })
    }
    
    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        if !self.enabled {
            return Ok(data.to_vec());
        }
        
        let mut nonce = [0u8; NONCE_LEN];
        self.rng.fill(&mut nonce).map_err(|e| anyhow!("Failed to generate nonce: {:?}", e))?;
        
        let sealing_key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.key)
                .map_err(|e| anyhow!("Failed to create key: {:?}", e))?
        );
        
        let mut in_out = data.to_vec();
        let tag = sealing_key.seal_in_place_separate_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut in_out,
        ).map_err(|e| anyhow!("Encryption failed: {:?}", e))?;
        
        let mut result = Vec::with_capacity(NONCE_LEN + in_out.len() + TAG_LEN);
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&in_out);
        result.extend_from_slice(tag.as_ref());
        
        Ok(result)
    }
    
    pub fn decrypt(&self, encrypted_data: &[u8]) -> Result<Vec<u8>> {
        if !self.enabled {
            return Ok(encrypted_data.to_vec());
        }
        
        if encrypted_data.len() < NONCE_LEN + TAG_LEN {
            return Err(anyhow!("Encrypted data too short"));
        }
        
        let nonce = &encrypted_data[..NONCE_LEN];
        let ciphertext_and_tag = &encrypted_data[NONCE_LEN..];
        
        let opening_key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.key)
                .map_err(|e| anyhow!("Failed to create key: {:?}", e))?
        );
        
        let mut in_out = ciphertext_and_tag.to_vec();
        let plaintext = opening_key.open_in_place(
            aead::Nonce::try_assume_unique_for_key(nonce)
                .map_err(|e| anyhow!("Invalid nonce: {:?}", e))?,
            aead::Aad::empty(),
            &mut in_out,
        ).map_err(|e| anyhow!("Decryption failed: {:?}", e))?;
        
        Ok(plaintext.to_vec())
    }
    
    pub fn encrypt_string(&self, data: &str) -> Result<String> {
        let encrypted = self.encrypt(data.as_bytes())?;
        Ok(general_purpose::STANDARD.encode(encrypted))
    }
    
    pub fn decrypt_string(&self, encrypted_data: &str) -> Result<String> {
        let decoded = general_purpose::STANDARD.decode(encrypted_data)
            .map_err(|e| anyhow!("Base64 decode error: {}", e))?;
        let decrypted = self.decrypt(&decoded)?;
        String::from_utf8(decrypted).map_err(|e| anyhow!("UTF-8 decode error: {}", e))
    }
}