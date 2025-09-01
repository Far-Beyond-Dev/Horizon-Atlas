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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
    /// Create a new proxy configuration with spatial regions
    pub fn new(listen_addr: &str, servers: Vec<(&str, &str)>) -> Result<Self> {
        let listen_addr = listen_addr.parse()?;
        let mut server_configs = Vec::new();
        let mut regions = Vec::new();
        
        if servers.is_empty() {
            return Err(ProxyError::Config("At least one server must be configured".to_string()));
        }
        
        // Create server configs and default spatial regions
        for (i, (addr_str, id)) in servers.iter().enumerate() {
            let addr = addr_str.parse()?;
            server_configs.push(ServerConfig::new(addr, id.to_string()));
            
            // Create regions in a linear arrangement for now
            // In production, this would be configured via file or database
            let region_coord = RegionCoordinate::new(i as i64, 0, 0);
            let center_x = (i as f64) * 2000.0; // 2000 units apart
            let center = WorldCoordinate::new(center_x, 0.0, 0.0);
            
            regions.push(ServerRegion::new(
                id.to_string(),
                region_coord,
                center,
                1000.0 // 1000 unit radius
            ));
        }
        
        Ok(Self {
            listen_addr,
            servers: server_configs,
            regions,
            buffer_size: 4096,
            max_connections: 10000,
            spatial_config: SpatialConfig::default(),
        })
    }
    
    /// Find the best server region for a given world position
    pub fn find_region_for_position(&self, position: &WorldCoordinate) -> Option<&ServerRegion> {
        // First, find regions that contain the position
        for region in &self.regions {
            if region.contains_point(position) {
                return Some(region);
            }
        }
        
        // If no region contains the position, find the closest one
        self.regions
            .iter()
            .min_by(|a, b| {
                let dist_a = a.center.distance_to(position);
                let dist_b = b.center.distance_to(position);
                dist_a.partial_cmp(&dist_b).unwrap_or(std::cmp::Ordering::Equal)
            })
    }
    
    /// Get server by ID
    pub fn get_server_by_id(&self, server_id: &str) -> Option<&ServerConfig> {
        self.servers.iter().find(|s| s.id == server_id)
    }
    
    /// Get region by server ID
    pub fn get_region_by_server_id(&self, server_id: &str) -> Option<&ServerRegion> {
        self.regions.iter().find(|r| r.server_id == server_id)
    }
    
    /// Find regions that a player is approaching
    pub fn find_approaching_regions(&self, position: &WorldCoordinate, velocity: &WorldCoordinate) -> Vec<&ServerRegion> {
        let prediction_time = self.spatial_config.prediction_time;
        let threshold = self.spatial_config.boundary_threshold;
        
        // Predict future position
        let predicted_pos = WorldCoordinate::new(
            position.x + velocity.x * prediction_time,
            position.y + velocity.y * prediction_time,
            position.z + velocity.z * prediction_time,
        );
        
        // Find regions that the player is approaching
        self.regions
            .iter()
            .filter(|region| {
                // Check if predicted position is within threshold of region boundary
                region.distance_to_boundary(&predicted_pos) <= threshold
            })
            .collect()
    }
    
    /// Check if a position is near any region boundary
    pub fn is_near_boundary(&self, position: &WorldCoordinate) -> Option<(&ServerRegion, f64)> {
        let threshold = self.spatial_config.boundary_threshold;
        
        for region in &self.regions {
            let distance = region.distance_to_boundary(position);
            if distance <= threshold {
                return Some((region, distance));
            }
        }
        
        None
    }
    
    /// Get all adjacent regions to a given region
    pub fn get_adjacent_regions(&self, region_coord: &RegionCoordinate) -> Vec<&ServerRegion> {
        let adjacent_coords = region_coord.adjacent_regions();
        
        self.regions
            .iter()
            .filter(|r| adjacent_coords.contains(&r.region_coord))
            .collect()
    }
}