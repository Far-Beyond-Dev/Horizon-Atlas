use std::net::SocketAddr;
use crate::error::{ProxyError, Result};

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
    /// Available backend servers
    pub servers: Vec<ServerConfig>,
    /// Buffer size for data transfers
    pub buffer_size: usize,
    /// Maximum concurrent connections
    pub max_connections: u32,
    /// Load balancing algorithm
    pub load_balance_algorithm: LoadBalanceAlgorithm,
}

#[derive(Debug)]
pub enum LoadBalanceAlgorithm {
    RoundRobin,
    LeastConnections,
    Random,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:9000".parse().unwrap(),
            servers: vec![
                ServerConfig::new("127.0.0.1:8080".parse().unwrap(), "server-1".to_string()),
                ServerConfig::new("127.0.0.1:8081".parse().unwrap(), "server-2".to_string()),
            ],
            buffer_size: 4096,
            max_connections: 10000,
            load_balance_algorithm: LoadBalanceAlgorithm::LeastConnections,
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