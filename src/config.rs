use std::net::SocketAddr;
use crate::error::{ProxyError, Result};
use crate::spatial::{ServerRegion, RegionCoordinate, WorldCoordinate};

/// Server configuration with load balancing information
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Server address
    pub addr: SocketAddr,
    /// Server identifier
    pub id: String,
    /// Current load (0.0 to 1.0)
    pub load: f32,
    /// Server capacity
    pub capacity: u32,
    /// Active connections
    pub active_connections: u32,
    /// Server health status
    pub healthy: bool,
}

impl ServerConfig {
    pub fn new(addr: SocketAddr, id: String) -> Self {
        Self {
            addr,
            id,
            load: 0.0,
            capacity: 1000,
            active_connections: 0,
            healthy: true,
        }
    }

    /// Calculate server load percentage
    pub fn load_percentage(&self) -> f32 {
        if self.capacity == 0 {
            1.0
        } else {
            self.active_connections as f32 / self.capacity as f32
        }
    }

    /// Check if server can accept new connections
    pub fn can_accept_connections(&self) -> bool {
        self.healthy && self.active_connections < self.capacity
    }
}

/// Proxy configuration
#[derive(Debug)]
pub struct ProxyConfig {
    /// Address to listen on
    pub listen_addr: SocketAddr,
    /// Available backend servers with spatial regions
    pub servers: Vec<ServerConfig>,
    /// Server regions for spatial routing
    pub regions: Vec<ServerRegion>,
    /// Buffer size for data transfers
    pub buffer_size: usize,
    /// Maximum concurrent connections
    pub max_connections: u32,
    /// Spatial routing settings
    pub spatial_config: SpatialConfig,
}

/// Spatial routing configuration
#[derive(Debug)]
pub struct SpatialConfig {
    /// Default region size (bounds radius in world units)
    pub default_region_size: f64,
    /// Transfer prediction time (seconds ahead to predict)
    pub prediction_time: f64,
    /// Boundary approach threshold for early transfer (world units)
    pub boundary_threshold: f64,
    /// Player position update file
    pub player_data_file: String,
    /// Auto-save interval for player data (seconds)
    pub auto_save_interval: u64,
}

impl Default for SpatialConfig {
    fn default() -> Self {
        Self {
            default_region_size: 1000.0, // 1000 unit radius per region
            prediction_time: 5.0, // Predict 5 seconds ahead
            boundary_threshold: 100.0, // Start transfer when 100 units from boundary
            player_data_file: "player_positions.json".to_string(),
            auto_save_interval: 30, // Save every 30 seconds
        }
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        let servers = vec![
            ServerConfig::new("127.0.0.1:8080".parse().unwrap(), "game-server-1".to_string()),
            ServerConfig::new("127.0.0.1:8081".parse().unwrap(), "game-server-2".to_string()),
            ServerConfig::new("127.0.0.1:8082".parse().unwrap(), "game-server-3".to_string()),
        ];
        
        // Create default spatial regions in a 3x3 grid around center (0,0,0)
        let regions = vec![
            // Center region
            ServerRegion::new(
                "game-server-1".to_string(),
                RegionCoordinate::new(0, 0, 0),
                WorldCoordinate::new(0.0, 0.0, 0.0),
                1000.0
            ),
            // Adjacent regions
            ServerRegion::new(
                "game-server-2".to_string(),
                RegionCoordinate::new(1, 0, 0),
                WorldCoordinate::new(2000.0, 0.0, 0.0),
                1000.0
            ),
            ServerRegion::new(
                "game-server-3".to_string(),
                RegionCoordinate::new(-1, 0, 0),
                WorldCoordinate::new(-2000.0, 0.0, 0.0),
                1000.0
            ),
        ];
        
        Self {
            listen_addr: "0.0.0.0:9000".parse().unwrap(),
            servers,
            regions,
            buffer_size: 4096,
            max_connections: 10000,
            spatial_config: SpatialConfig::default(),
        }
    }
}

impl ProxyConfig {
    /// Create a new proxy configuration
    pub fn new(listen_addr: &str, servers: Vec<(&str, &str)>) -> Result<Self> {
        let listen_addr = listen_addr.parse()?;
        let mut server_configs = Vec::new();
        
        for (addr_str, id) in servers {
            let addr = addr_str.parse()?;
            server_configs.push(ServerConfig::new(addr, id.to_string()));
        }
        
        if server_configs.is_empty() {
            return Err(ProxyError::Config("At least one server must be configured".to_string()));
        }
        
        Ok(Self {
            listen_addr,
            servers: server_configs,
            buffer_size: 4096,
            max_connections: 10000,
            load_balance_algorithm: LoadBalanceAlgorithm::LeastConnections,
        })
    }
    
    /// Get the best available server based on load balancing algorithm
    pub fn get_best_server(&self) -> Option<&ServerConfig> {
        let available_servers: Vec<&ServerConfig> = self.servers
            .iter()
            .filter(|s| s.can_accept_connections())
            .collect();
            
        if available_servers.is_empty() {
            return None;
        }
        
        match self.load_balance_algorithm {
            LoadBalanceAlgorithm::LeastConnections => {
                available_servers
                    .iter()
                    .min_by_key(|s| s.active_connections)
                    .copied()
            }
            LoadBalanceAlgorithm::RoundRobin => {
                // Simple implementation - in production, you'd track the last used server
                available_servers.first().copied()
            }
            LoadBalanceAlgorithm::Random => {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                
                let mut hasher = DefaultHasher::new();
                std::thread::current().id().hash(&mut hasher);
                let hash = hasher.finish();
                let index = (hash as usize) % available_servers.len();
                available_servers.get(index).copied()
            }
        }
    }
}