use crate::errors::{AtlasError, Result};
use crate::types::RegionBounds;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub cluster: ClusterConfig,
    pub crypto: CryptoConfig,
    pub discovery: DiscoveryConfig,
    pub proxy: ProxyConfig,
    pub routing: RoutingConfig,
    pub health: HealthConfig,
    pub metrics: MetricsConfig,
    pub regions: RegionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub bind_address: SocketAddr,
    pub max_connections: u32,
    pub connection_timeout: u64,
    pub keepalive_interval: u64,
    pub max_message_size: u32,
    pub buffer_size: u32,
    pub worker_threads: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    pub node_id: String,
    pub cluster_name: String,
    pub seed_nodes: Vec<SocketAddr>,
    pub election_timeout: u64,
    pub heartbeat_interval: u64,
    pub max_retry_attempts: u32,
    pub consensus_algorithm: ConsensusAlgorithm,
    pub replication_factor: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsensusAlgorithm {
    Raft,
    Pbft,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoConfig {
    pub enabled: bool,
    pub key_rotation_interval: u64,
    pub cipher_suite: CipherSuite,
    pub key_derivation: KeyDerivationConfig,
    pub certificate_path: Option<String>,
    pub private_key_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CipherSuite {
    ChaCha20Poly1305,
    Aes256Gcm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyDerivationConfig {
    pub algorithm: String,
    pub iterations: u32,
    pub salt_length: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    pub service_name: String,
    pub discovery_interval: u64,
    pub service_ttl: u64,
    pub health_check_interval: u64,
    pub backend: DiscoveryBackend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiscoveryBackend {
    Etcd { endpoints: Vec<String> },
    Consul { endpoint: String },
    Static { servers: Vec<StaticServer> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticServer {
    pub id: String,
    pub address: SocketAddr,
    pub gameworld_bounds: RegionBounds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub buffer_size: u32,
    pub max_concurrent_connections: u32,
    pub connection_pool_size: u32,
    pub retry_attempts: u32,
    pub retry_delay: u64,
    pub connection_timeout: u64,
    pub compression: CompressionConfig,
    pub rate_limiting: RateLimitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    pub enabled: bool,
    pub algorithm: CompressionAlgorithm,
    pub level: u8,
    pub min_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompressionAlgorithm {
    Lz4,
    Zstd,
    Gzip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub requests_per_second: u32,
    pub burst_size: u32,
    pub window_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    pub load_balancing: LoadBalancingConfig,
    pub failover: FailoverConfig,
    pub session_affinity: bool,
    pub region_switching_threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancingConfig {
    pub algorithm: LoadBalancingAlgorithm,
    pub health_check_weight: f32,
    pub load_weight: f32,
    pub latency_weight: f32,
    pub connection_weight: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoadBalancingAlgorithm {
    RoundRobin,
    WeightedRoundRobin,
    LeastConnections,
    LeastResponseTime,
    ConsistentHashing,
    Geographic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailoverConfig {
    pub enabled: bool,
    pub health_check_timeout: u64,
    pub max_failures: u32,
    pub backoff_multiplier: f32,
    pub recovery_check_interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    pub check_interval: u64,
    pub timeout: u64,
    pub failure_threshold: u32,
    pub success_threshold: u32,
    pub metrics_retention: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub bind_address: SocketAddr,
    pub collection_interval: u64,
    pub retention_period: u64,
    pub exporters: Vec<MetricsExporter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetricsExporter {
    Prometheus { endpoint: String },
    InfluxDb { endpoint: String, database: String },
    DataDog { api_key: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionConfig {
    pub auto_scaling: AutoScalingConfig,
    pub transition_zone_size: f64,
    pub max_players_per_region: u32,
    pub region_definitions: HashMap<String, RegionBounds>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoScalingConfig {
    pub enabled: bool,
    pub scale_up_threshold: f32,
    pub scale_down_threshold: f32,
    pub cooldown_period: u64,
    pub min_servers_per_region: u32,
    pub max_servers_per_region: u32,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = std::env::var("ATLAS_CONFIG").unwrap_or_else(|_| "atlas.toml".to_string());
        Self::load_from_file(&config_path)
    }
    
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| AtlasError::Config(format!("Failed to read config file: {}", e)))?;
        
        let config: Config = toml::from_str(&content)
            .map_err(|e| AtlasError::Config(format!("Failed to parse config: {}", e)))?;
        
        config.validate()?;
        Ok(config)
    }
    
    pub fn default() -> Self {
        Self {
            server: ServerConfig {
                bind_address: "0.0.0.0:8080".parse().unwrap(),
                max_connections: 10000,
                connection_timeout: 30000,
                keepalive_interval: 30000,
                max_message_size: 1024 * 1024,
                buffer_size: 8192,
                worker_threads: None,
            },
            cluster: ClusterConfig {
                node_id: uuid::Uuid::new_v4().to_string(),
                cluster_name: "horizon-atlas".to_string(),
                seed_nodes: vec!["127.0.0.1:8081".parse().unwrap()],
                election_timeout: 5000,
                heartbeat_interval: 1000,
                max_retry_attempts: 3,
                consensus_algorithm: ConsensusAlgorithm::Raft,
                replication_factor: 3,
            },
            crypto: CryptoConfig {
                enabled: true,
                key_rotation_interval: 3600000,
                cipher_suite: CipherSuite::ChaCha20Poly1305,
                key_derivation: KeyDerivationConfig {
                    algorithm: "PBKDF2".to_string(),
                    iterations: 100000,
                    salt_length: 32,
                },
                certificate_path: None,
                private_key_path: None,
            },
            discovery: DiscoveryConfig {
                service_name: "horizon-game-server".to_string(),
                discovery_interval: 10000,
                service_ttl: 30000,
                health_check_interval: 5000,
                backend: DiscoveryBackend::Static {
                    servers: vec![],
                },
            },
            proxy: ProxyConfig {
                buffer_size: 8192,
                max_concurrent_connections: 1000,
                connection_pool_size: 100,
                retry_attempts: 3,
                retry_delay: 1000,
                connection_timeout: 30000,
                compression: CompressionConfig {
                    enabled: true,
                    algorithm: CompressionAlgorithm::Lz4,
                    level: 4,
                    min_size: 1024,
                },
                rate_limiting: RateLimitConfig {
                    enabled: true,
                    requests_per_second: 100,
                    burst_size: 200,
                    window_size: 60000,
                },
            },
            routing: RoutingConfig {
                load_balancing: LoadBalancingConfig {
                    algorithm: LoadBalancingAlgorithm::ConsistentHashing,
                    health_check_weight: 0.4,
                    load_weight: 0.3,
                    latency_weight: 0.2,
                    connection_weight: 0.1,
                },
                failover: FailoverConfig {
                    enabled: true,
                    health_check_timeout: 5000,
                    max_failures: 3,
                    backoff_multiplier: 2.0,
                    recovery_check_interval: 30000,
                },
                session_affinity: true,
                region_switching_threshold: 10.0,
            },
            health: HealthConfig {
                check_interval: 30000,
                timeout: 5000,
                failure_threshold: 3,
                success_threshold: 2,
                metrics_retention: 3600000,
            },
            metrics: MetricsConfig {
                enabled: true,
                bind_address: "0.0.0.0:9090".parse().unwrap(),
                collection_interval: 5000,
                retention_period: 86400000,
                exporters: vec![],
            },
            regions: RegionConfig {
                auto_scaling: AutoScalingConfig {
                    enabled: true,
                    scale_up_threshold: 0.8,
                    scale_down_threshold: 0.3,
                    cooldown_period: 300000,
                    min_servers_per_region: 1,
                    max_servers_per_region: 10,
                },
                transition_zone_size: 50.0,
                max_players_per_region: 1000,
                region_definitions: HashMap::new(),
            },
        }
    }
    
    fn validate(&self) -> Result<()> {
        if self.server.max_connections == 0 {
            return Err(AtlasError::Config("max_connections must be greater than 0".to_string()));
        }
        
        if self.server.connection_timeout == 0 {
            return Err(AtlasError::Config("connection_timeout must be greater than 0".to_string()));
        }
        
        if self.cluster.replication_factor == 0 {
            return Err(AtlasError::Config("replication_factor must be greater than 0".to_string()));
        }
        
        if self.health.failure_threshold == 0 {
            return Err(AtlasError::Config("failure_threshold must be greater than 0".to_string()));
        }
        
        if self.health.success_threshold == 0 {
            return Err(AtlasError::Config("success_threshold must be greater than 0".to_string()));
        }
        
        Ok(())
    }
}