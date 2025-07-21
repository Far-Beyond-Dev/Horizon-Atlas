use anyhow::{Result, anyhow};
use lz4_flex::{compress_prepend_size, decompress_size_prepended};

#[derive(Clone)]
pub struct Compressor {
    threshold: usize,
    enabled: bool,
}

impl Compressor {
    pub fn new(enabled: bool, threshold: usize) -> Self {
        Self { enabled, threshold }
    }
    
    pub fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        if !self.enabled || data.len() < self.threshold {
            return Ok(data.to_vec());
        }
        
        let compressed = compress_prepend_size(data);
        Ok(compressed)
    }
    
    pub fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        if !self.enabled || data.len() < 4 {
            return Ok(data.to_vec());
        }
        
        let original_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        
        if original_size < self.threshold {
            return Ok(data.to_vec());
        }
        
        let decompressed = decompress_size_prepended(data)
            .map_err(|e| anyhow!("Decompression failed: {}", e))?;
        
        Ok(decompressed)
    }
    
    pub fn estimate_compression_ratio(&self, data: &[u8]) -> f32 {
        if !self.enabled || data.len() < self.threshold {
            return 1.0;
        }
        
        let compressed = compress_prepend_size(data);
        compressed.len() as f32 / data.len() as f32
    }
}

#[derive(Clone)]
pub struct DeltaCompressor {
    compressor: Compressor,
}

impl DeltaCompressor {
    pub fn new(enabled: bool, threshold: usize) -> Self {
        Self {
            compressor: Compressor::new(enabled, threshold),
        }
    }
    
    pub fn create_delta(&self, old_state: &[u8], new_state: &[u8]) -> Result<Vec<u8>> {
        
        let min_len = old_state.len().min(new_state.len());
        let mut changes = Vec::new();
        
        for i in 0..min_len {
            if old_state[i] != new_state[i] {
                changes.push((i, new_state[i]));
            }
        }
        
        if new_state.len() > old_state.len() {
            for i in old_state.len()..new_state.len() {
                changes.push((i, new_state[i]));
            }
        }
        
        let serialized_changes = bincode::serialize(&changes)?;
        self.compressor.compress(&serialized_changes)
    }
    
    pub fn apply_delta(&self, base_state: &mut Vec<u8>, delta: &[u8]) -> Result<()> {
        let decompressed = self.compressor.decompress(delta)?;
        let changes: Vec<(usize, u8)> = bincode::deserialize(&decompressed)?;
        
        for (index, value) in changes {
            if index >= base_state.len() {
                base_state.resize(index + 1, 0);
            }
            base_state[index] = value;
        }
        
        Ok(())
    }
}