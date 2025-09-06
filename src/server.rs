use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use crate::config::{ServerConfig, ProxyConfig};
use crate::spatial::{RegionCoordinate, WorldCoordinate, ServerRegion, MovementData};
use crate::error::{ProxyError, Result};

/// Server connection manager with spatial routing
#[derive(Debug)]
pub struct ServerManager {
    /// Available servers
    pub servers: Arc<Mutex<HashMap<String, ServerConfig>>>,
    /// Spatial regions mapped to servers
    pub regions: Arc<Mutex<HashMap<RegionCoordinate, ServerRegion>>>,
    /// Player movement tracking
    pub player_movements: Arc<Mutex<HashMap<String, MovementData>>>,
    /// Configuration
    pub config: ProxyConfig,
}

impl ServerManager {
    /// Create a new server manager with spatial configuration
    pub fn new(config: ProxyConfig) -> Self {
        let server_map: HashMap<String, ServerConfig> = config.servers
            .iter()
            .map(|s| (s.id.clone(), s.clone()))
            .collect();
            
        let region_map: HashMap<RegionCoordinate, ServerRegion> = config.regions
            .iter()
            .map(|r| (r.region_coord, r.clone()))
            .collect();
            
        Self {
            servers: Arc::new(Mutex::new(server_map)),
            regions: Arc::new(Mutex::new(region_map)),
            player_movements: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }
    
    /// Create a new server manager (legacy constructor)
    pub fn from_servers(servers: Vec<ServerConfig>) -> Self {
        let config = ProxyConfig {
            servers: servers.clone(),
            regions: vec![],
            ..Default::default()
        };
        Self::new(config)
    }
    
    /// Connect to a specific server
    pub fn connect_to_server(&self, server_id: &str) -> Result<TcpStream> {
        let servers = self.servers.lock().unwrap();
        let server = servers.get(server_id)
            .ok_or_else(|| ProxyError::Connection(format!("Server {} not found", server_id)))?;
            
        if !server.healthy {
            return Err(ProxyError::Connection(format!("Server {} is unhealthy", server_id)));
        }
        
        TcpStream::connect(server.addr)
            .map_err(|e| ProxyError::Connection(format!("Failed to connect to server {}: {}", server_id, e)))
    }
    
    /// Get server by ID
    pub fn get_server(&self, server_id: &str) -> Option<ServerConfig> {
        self.servers.lock().unwrap().get(server_id).cloned()
    }
    
    /// Update server connection count
    pub fn increment_connections(&self, server_id: &str) -> Result<()> {
        let mut servers = self.servers.lock().unwrap();
        if let Some(server) = servers.get_mut(server_id) {
            server.active_connections += 1;
            server.load = server.load_percentage();
            Ok(())
        } else {
            Err(ProxyError::Connection(format!("Server {} not found", server_id)))
        }
    }
    
    /// Decrement server connection count
    pub fn decrement_connections(&self, server_id: &str) -> Result<()> {
        let mut servers = self.servers.lock().unwrap();
        if let Some(server) = servers.get_mut(server_id) {
            if server.active_connections > 0 {
                server.active_connections -= 1;
            }
            server.load = server.load_percentage();
            Ok(())
        } else {
            Err(ProxyError::Connection(format!("Server {} not found", server_id)))
        }
    }
    
    /// Get the best server for a player spawning at (0, 0, 0)
    pub fn get_spawn_server(&self) -> Option<ServerConfig> {
        self.get_server_for_position(&WorldCoordinate::new(0.0, 0.0, 0.0))
    }
    
    /// Get the best server for a specific world position
    pub fn get_server_for_position(&self, position: &WorldCoordinate) -> Option<ServerConfig> {
        let regions = self.regions.lock().unwrap();
        let servers = self.servers.lock().unwrap();
        
        // First, try to find a region that contains this position
        for region in regions.values() {
            if region.contains_point(position) && region.healthy {
                if let Some(server) = servers.get(&region.server_id) {
                    if server.can_accept_connections() {
                        return Some(server.clone());
                    }
                }
            }
        }
        
        // TODO: Call server deployment API to request new instance for this position
        // This will eventually spawn a new server instance to handle the uncovered region
        None
    }
    
    /// Get the best available server for new connections (legacy method)
    pub fn get_best_server(&self) -> Option<ServerConfig> {
        self.get_spawn_server()
    }
    
    /// Update server health status
    pub fn update_server_health(&self, server_id: &str, healthy: bool) -> Result<()> {
        let mut servers = self.servers.lock().unwrap();
        if let Some(server) = servers.get_mut(server_id) {
            server.healthy = healthy;
            Ok(())
        } else {
            Err(ProxyError::Connection(format!("Server {} not found", server_id)))
        }
    }
    
    /// Get all server statistics
    pub fn get_server_stats(&self) -> Vec<ServerConfig> {
        self.servers.lock().unwrap().values().cloned().collect()
    }
    
    /// Health check for a server
    pub fn health_check(&self, server_id: &str) -> bool {
        if let Some(server) = self.get_server(server_id) {
            // Simple TCP connection test
            match TcpStream::connect(server.addr) {
                Ok(_) => {
                    let _ = self.update_server_health(server_id, true);
                    true
                }
                Err(_) => {
                    let _ = self.update_server_health(server_id, false);
                    false
                }
            }
        } else {
            false
        }
    }
    
    /// Get server load for a specific server
    pub fn get_server_load(&self, server_id: &str) -> f32 {
        self.servers.lock().unwrap()
            .get(server_id)
            .map(|s| s.load_percentage())
            .unwrap_or(1.0)
    }
    
    /// Check if server can accept new connections
    pub fn can_accept_connections(&self, server_id: &str) -> bool {
        self.servers.lock().unwrap()
            .get(server_id)
            .map(|s| s.can_accept_connections())
            .unwrap_or(false)
    }
    
    /// Initialize player movement tracking at spawn position (0, 0, 0)
    pub fn initialize_player(&self, player_id: &str) -> Result<()> {
        let spawn_position = WorldCoordinate::new(0.0, 0.0, 0.0);
        let movement_data = MovementData::new(spawn_position);
        
        let mut players = self.player_movements.lock().unwrap();
        players.insert(player_id.to_string(), movement_data);
        
        Ok(())
    }
    
    /// Update player position and check for region transfers
    pub fn update_player_position(&self, player_id: &str, position: WorldCoordinate) -> Result<Option<String>> {
        let mut players = self.player_movements.lock().unwrap();
        
        if let Some(movement_data) = players.get_mut(player_id) {
            let old_position = movement_data.position;
            movement_data.update_position(position);
            
            drop(players); // Release the lock
            
            // Check if player has moved to a different region
            let old_region = self.get_region_for_position(&old_position);
            let new_region = self.get_region_for_position(&position);
            
            match (old_region, new_region) {
                (Some(old_r), Some(new_r)) if old_r.server_id != new_r.server_id => {
                    // Player moved to a different server region
                    Ok(Some(new_r.server_id.clone()))
                },
                _ => Ok(None) // No server change needed
            }
        } else {
            Err(ProxyError::Connection(format!("Player {} not found", player_id)))
        }
    }
    
    /// Get the region that contains a specific position
    pub fn get_region_for_position(&self, position: &WorldCoordinate) -> Option<ServerRegion> {
        let regions = self.regions.lock().unwrap();
        
        // First, find regions that contain the position
        for region in regions.values() {
            if region.contains_point(position) {
                return Some(region.clone());
            }
        }
        
        // If no region contains the position, find the closest one
        regions
            .values()
            .min_by(|a, b| {
                let dist_a = a.center.distance_to(position);
                let dist_b = b.center.distance_to(position);
                dist_a.partial_cmp(&dist_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
    }
    
    /// Get player's current position
    pub fn get_player_position(&self, player_id: &str) -> Option<WorldCoordinate> {
        let players = self.player_movements.lock().unwrap();
        players.get(player_id).map(|m| m.position)
    }
    
    /// Get position relative to region center
    pub fn get_relative_position(&self, player_id: &str) -> Option<(WorldCoordinate, RegionCoordinate)> {
        let players = self.player_movements.lock().unwrap();
        if let Some(movement_data) = players.get(player_id) {
            let position = movement_data.position;
            drop(players);
            
            if let Some(region) = self.get_region_for_position(&position) {
                let relative_pos = WorldCoordinate::new(
                    position.x - region.center.x,
                    position.y - region.center.y,
                    position.z - region.center.z,
                );
                return Some((relative_pos, region.region_coord));
            }
        }
        None
    }
    
    /// Remove player from tracking
    pub fn remove_player(&self, player_id: &str) {
        let mut players = self.player_movements.lock().unwrap();
        players.remove(player_id);
    }
    
    /// Get regions that a player might transfer to based on movement prediction
    pub fn get_approaching_regions(&self, player_id: &str) -> Vec<ServerRegion> {
        let players = self.player_movements.lock().unwrap();
        if let Some(movement_data) = players.get(player_id) {
            let prediction_time = self.config.spatial_config.prediction_time;
            let threshold = self.config.spatial_config.boundary_threshold;
            
            let predicted_pos = movement_data.predict_position(prediction_time);
            drop(players);
            
            let regions = self.regions.lock().unwrap();
            regions
                .values()
                .filter(|region| {
                    region.distance_to_boundary(&predicted_pos) <= threshold
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }
    
    /// Request deployment of a new server instance for a specific world position
    /// This will be implemented to call the server deployment API
    pub fn request_server_deployment(&self, position: &WorldCoordinate) -> Result<()> {
        // TODO: Implement server deployment API call
        // This should:
        // 1. Calculate which region coordinate this position should belong to
        // 2. Call deployment API to spawn new server instance at that region
        // 3. Add the new server and region to our maps once deployment succeeds
        // 4. Return appropriate errors for deployment failures
        
        // For now, return an error indicating this feature is not yet implemented
        Err(ProxyError::Connection(
            format!("Server deployment not yet implemented for position ({}, {}, {})", 
                   position.x, position.y, position.z)
        ))
    }
}