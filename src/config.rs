use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use tracing::{info, error, debug};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub cluster: ClusterConfig,
    pub security: SecurityConfig,
    pub game_servers: GameServerConfig,
    pub spatial: SpatialConfig,  // Renamed from regions
    pub compression: CompressionConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub address: String,
    pub port: u16,
    pub max_connections: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    pub node_id: String,
    pub discovery_interval: u64,
    pub health_check_interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub enable_encryption: bool,
    pub key_rotation_interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameServerConfig {
    pub startup_timeout: u64,
    pub shutdown_timeout: u64,
    pub max_idle_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpatialConfig {
    pub preload_distance: f64,
    pub transfer_distance: f64,
    pub sync_interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    pub enable: bool,
    pub algorithm: String,
    pub threshold: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
}

impl Config {
    pub async fn load(path: &str) -> Result<Self> {
        info!("Loading configuration from: {}", path);
        
        let contents = fs::read_to_string(path)
            .map_err(|e| {
                error!("Failed to read config file '{}': {}", path, e);
                e
            })?;
            
        debug!("Configuration file size: {} bytes", contents.len());
        
        let config: Config = toml::from_str(&contents)
            .map_err(|e| {
                error!("Failed to parse configuration: {}", e);
                e
            })?;
            
        // Log configuration summary (without sensitive data)
        info!(
            "Configuration loaded successfully - Node: {}, Server: {}:{}, Encryption: {}, Compression: {}",
            config.cluster.node_id,
            config.server.address,
            config.server.port,
            config.security.enable_encryption,
            config.compression.enable
        );
        
        debug!(
            "Config details - Max connections: {}, Preload distance: {}, Transfer distance: {}",
            config.server.max_connections,
            config.spatial.preload_distance,
            config.spatial.transfer_distance
        );
        
        Ok(config)
    }
}