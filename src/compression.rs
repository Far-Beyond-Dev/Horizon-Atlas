use anyhow::{Result, anyhow};
use lz4_flex::{compress_prepend_size, decompress_size_prepended};
use std::time::Instant;
use tracing::{info, warn, error, debug};

#[derive(Clone)]
pub struct Compressor {
    threshold: usize,
    enabled: bool,
}

impl Compressor {
    pub fn new(enabled: bool, threshold: usize) -> Self {
        info!("Initializing compressor (enabled: {}, threshold: {} bytes)", enabled, threshold);
        Self { enabled, threshold }
    }
    
    pub fn compress(&self, data: &[u8]) -> Result<Vec<u8>> {
        let start_time = Instant::now();
        let original_size = data.len();
        
        debug!("Starting compression of {} bytes", original_size);
        
        if !self.enabled {
            debug!("Compression bypassed (disabled)");
            return Ok(data.to_vec());
        }
        
        if data.len() < self.threshold {
            debug!("Compression bypassed (below threshold: {} < {})", data.len(), self.threshold);
            return Ok(data.to_vec());
        }
        
        let compressed = compress_prepend_size(data);
        let compressed_size = compressed.len();
        let compression_time = start_time.elapsed();
        let ratio = compressed_size as f32 / original_size as f32;
        let savings = original_size.saturating_sub(compressed_size);
        
        info!(
            "Compression completed: {} bytes -> {} bytes (ratio: {:.2}, saved: {} bytes) in {:?}",
            original_size, compressed_size, ratio, savings, compression_time
        );
        
        if ratio > 0.9 {
            warn!("Poor compression ratio: {:.2} for {} bytes", ratio, original_size);
        }
        
        if compression_time.as_millis() > 5 {
            warn!("Compression took longer than expected: {:?} for {} bytes", compression_time, original_size);
        }
        
        debug!("Compression operation completed successfully");
        
        Ok(compressed)
    }
    
    pub fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        let start_time = Instant::now();
        let compressed_size = data.len();
        
        debug!("Starting decompression of {} bytes", compressed_size);
        
        if !self.enabled {
            debug!("Decompression bypassed (disabled)");
            return Ok(data.to_vec());
        }
        
        if data.len() < 4 {
            debug!("Decompression bypassed (data too small: {} bytes)", data.len());
            return Ok(data.to_vec());
        }
        
        let original_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        
        if original_size < self.threshold {
            debug!("Decompression bypassed (original below threshold: {} < {})", original_size, self.threshold);
            return Ok(data.to_vec());
        }
        
        let decompressed = decompress_size_prepended(data)
            .map_err(|e| {
                error!("Decompression failed for {} bytes - possible data corruption: {}", compressed_size, e);
                anyhow!("Decompression failed: {}", e)
            })?;
            
        let decompression_time = start_time.elapsed();
        let decompressed_size = decompressed.len();
        let ratio = compressed_size as f32 / decompressed_size as f32;
        
        info!(
            "Decompression completed: {} bytes -> {} bytes (ratio: {:.2}) in {:?}",
            compressed_size, decompressed_size, ratio, decompression_time
        );
        
        if decompression_time.as_millis() > 5 {
            warn!("Decompression took longer than expected: {:?} for {} bytes", decompression_time, compressed_size);
        }
        
        debug!("Decompression operation completed successfully");
        
        Ok(decompressed)
    }
    
    pub fn estimate_compression_ratio(&self, data: &[u8]) -> f32 {
        let start_time = Instant::now();
        let original_size = data.len();
        
        debug!("Estimating compression ratio for {} bytes", original_size);
        
        if !self.enabled || data.len() < self.threshold {
            debug!("Compression estimation bypassed (disabled or below threshold)");
            return 1.0;
        }
        
        let compressed = compress_prepend_size(data);
        let ratio = compressed.len() as f32 / data.len() as f32;
        let estimation_time = start_time.elapsed();
        
        debug!(
            "Compression ratio estimated: {:.3} for {} bytes in {:?}",
            ratio, original_size, estimation_time
        );
        
        if estimation_time.as_millis() > 10 {
            warn!("Compression estimation took longer than expected: {:?} for {} bytes", estimation_time, original_size);
        }
        
        ratio
    }
}

#[derive(Clone)]
pub struct DeltaCompressor {
    compressor: Compressor,
}

impl DeltaCompressor {
    pub fn new(enabled: bool, threshold: usize) -> Self {
        info!("Initializing delta compressor (enabled: {}, threshold: {} bytes)", enabled, threshold);
        Self {
            compressor: Compressor::new(enabled, threshold),
        }
    }
    
    pub fn create_delta(&self, old_state: &[u8], new_state: &[u8]) -> Result<Vec<u8>> {
        let start_time = Instant::now();
        let old_size = old_state.len();
        let new_size = new_state.len();
        
        debug!("Creating delta: {} bytes -> {} bytes", old_size, new_size);
        
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
        
        let change_count = changes.len();
        let serialized_changes = bincode::serialize(&changes)
            .map_err(|e| {
                error!("Delta serialization failed for {} changes: {}", change_count, e);
                anyhow!("Delta serialization failed: {}", e)
            })?;
            
        let serialized_size = serialized_changes.len();
        let compressed_delta = self.compressor.compress(&serialized_changes)?;
        let delta_time = start_time.elapsed();
        let final_delta_size = compressed_delta.len();
        
        let efficiency = if new_size > 0 {
            final_delta_size as f32 / new_size as f32
        } else {
            0.0
        };
        
        info!(
            "Delta created: {} changes, {} bytes serialized -> {} bytes compressed (efficiency: {:.2}) in {:?}",
            change_count, serialized_size, final_delta_size, efficiency, delta_time
        );
        
        if efficiency > 0.5 {
            warn!("Delta is inefficient ({:.2}): consider full state transfer for {} -> {} bytes", 
                  efficiency, old_size, new_size);
        }
        
        if delta_time.as_millis() > 50 {
            warn!("Delta creation took longer than expected: {:?} for {} changes", delta_time, change_count);
        }
        
        debug!("Delta creation completed successfully");
        
        Ok(compressed_delta)
    }
    
    pub fn apply_delta(&self, base_state: &mut Vec<u8>, delta: &[u8]) -> Result<()> {
        let start_time = Instant::now();
        let delta_size = delta.len();
        let initial_state_size = base_state.len();
        
        debug!("Applying delta: {} bytes delta to {} bytes state", delta_size, initial_state_size);
        
        let decompressed = self.compressor.decompress(delta)?;
        let changes: Vec<(usize, u8)> = bincode::deserialize(&decompressed)
            .map_err(|e| {
                error!("Delta deserialization failed for {} bytes: {}", decompressed.len(), e);
                anyhow!("Delta deserialization failed: {}", e)
            })?;
            
        let change_count = changes.len();
        let mut resize_count = 0;
        
        for (index, value) in changes {
            if index >= base_state.len() {
                base_state.resize(index + 1, 0);
                resize_count += 1;
            }
            base_state[index] = value;
        }
        
        let delta_time = start_time.elapsed();
        let final_state_size = base_state.len();
        let size_change = final_state_size as i64 - initial_state_size as i64;
        
        info!(
            "Delta applied: {} changes ({} resizes), {} bytes -> {} bytes ({:+} bytes) in {:?}",
            change_count, resize_count, initial_state_size, final_state_size, size_change, delta_time
        );
        
        if resize_count > change_count / 2 {
            warn!("High resize count: {} resizes for {} changes - consider state size optimization", 
                  resize_count, change_count);
        }
        
        if delta_time.as_millis() > 20 {
            warn!("Delta application took longer than expected: {:?} for {} changes", delta_time, change_count);
        }
        
        debug!("Delta application completed successfully");
        
        Ok(())
    }
}