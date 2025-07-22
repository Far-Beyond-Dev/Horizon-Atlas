use crate::state::{Vector3, GameServerInfo};
use anyhow::Result;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, warn, debug};
use uuid::Uuid;

pub struct ServerManager {
    // Track all servers and their spatial boundaries
    servers: Arc<DashMap<Uuid, GameServerInfo>>,
    player_positions: Arc<RwLock<HashMap<Uuid, Vector3>>>,
    player_servers: Arc<DashMap<Uuid, Uuid>>,  // player_id -> server_id
    preload_distance: f64,  // Distance for preloading nearby servers
    transfer_distance: f64, // Distance threshold for server transfers
}

impl ServerManager {
    pub fn new(preload_distance: f64, transfer_distance: f64) -> Self {
        info!(
            "Initializing server manager (preload: {:.2}, transfer: {:.2})", 
            preload_distance, transfer_distance
        );
        
        Self {
            servers: Arc::new(DashMap::new()),
            player_positions: Arc::new(RwLock::new(HashMap::new())),
            player_servers: Arc::new(DashMap::new()),
            preload_distance,
            transfer_distance,
        }
    }
    
    pub async fn register_server(&self, server_info: GameServerInfo) -> Result<()> {
        let server_id = server_info.id;
        let bounds = server_info.bounds.clone();
        
        let center_x = (bounds.min.x + bounds.max.x) / 2.0;
        let center_y = (bounds.min.y + bounds.max.y) / 2.0;
        let center_z = (bounds.min.z + bounds.max.z) / 2.0;
        
        info!(
            "Registering server '{}' with bounds (center: {:.2}, {:.2}, {:.2})",
            server_id, center_x, center_y, center_z
        );
        
        self.servers.insert(server_id, server_info);
        
        let server_count = self.servers.len();
        info!("Server registration completed - total servers: {}", server_count);
        
        let size_x = bounds.max.x - bounds.min.x;
        let size_y = bounds.max.y - bounds.min.y;
        let size_z = bounds.max.z - bounds.min.z;
        
        debug!(
            "Server bounds: size ({:.2}, {:.2}, {:.2})", 
            size_x, size_y, size_z
        );
        
        Ok(())
    }
    
    pub async fn unregister_server(&self, server_id: Uuid) -> Result<()> {
        info!("Unregistering server: {}", server_id);
        
        let was_present = self.servers.remove(&server_id).is_some();
        
        if !was_present {
            warn!("Attempted to unregister non-existent server: {}", server_id);
        }
        
        // Remove all players from this server
        let mut removed_players = 0;
        self.player_servers.retain(|_, v| {
            if *v == server_id {
                removed_players += 1;
                false
            } else {
                true
            }
        });
        
        let server_count = self.servers.len();
        
        info!(
            "Server unregistration completed - removed {} player assignments, {} servers remaining",
            removed_players, server_count
        );
        
        Ok(())
    }
    
    pub async fn update_player_position(&self, player_id: Uuid, position: Vector3, current_server: Uuid) -> Result<()> {
        debug!(
            "Updating player {} position to ({:.2}, {:.2}, {:.2}) on server {}",
            player_id, position.x, position.y, position.z, current_server
        );
        
        let mut positions = self.player_positions.write().await;
        let was_tracked = positions.insert(player_id, position).is_some();
        let total_players = positions.len();
        
        // Update player's current server
        self.player_servers.insert(player_id, current_server);
        
        if !was_tracked {
            info!("Started tracking new player {} on server {} - total tracked: {}", player_id, current_server, total_players);
        }
        
        debug!("Player position update completed for {}", player_id);
        
        Ok(())
    }
    
    pub async fn find_server_for_position(&self, position: &Vector3) -> Option<Uuid> {
        let start_time = Instant::now();
        
        debug!("Finding server for position ({:.2}, {:.2}, {:.2})", position.x, position.y, position.z);
        
        let server_count = self.servers.len();
        let mut checked_servers = 0;
        
        for server in self.servers.iter() {
            checked_servers += 1;
            if server.value().bounds.contains(position) {
                let search_time = start_time.elapsed();
                
                debug!(
                    "Found server '{}' for position after checking {}/{} servers in {:?}",
                    server.key(), checked_servers, server_count, search_time
                );
                
                if search_time.as_millis() > 10 {
                    warn!(
                        "Server search took longer than expected: {:?} to check {}/{} servers",
                        search_time, checked_servers, server_count
                    );
                }
                
                return Some(*server.key());
            }
        }
        
        let search_time = start_time.elapsed();
        debug!(
            "No server found for position after checking {} servers in {:?}",
            server_count, search_time
        );
        
        if server_count > 100 && search_time.as_millis() > 50 {
            warn!(
                "Exhaustive server search performance issue: {:?} for {} servers",
                search_time, server_count
            );
        }
        
        None
    }
    
    pub async fn should_transfer_player(&self, player_position: &Vector3, current_server: Uuid) -> Option<Uuid> {
        let start_time = Instant::now();
        
        debug!(
            "Checking transfer necessity for player on server '{}' at ({:.2}, {:.2}, {:.2})",
            current_server, player_position.x, player_position.y, player_position.z
        );
        
        let mut closest_server: Option<(Uuid, f64)> = None;
        let server_count = self.servers.len();
        let mut evaluated_servers = 0;
        let mut candidates_found = 0;
        
        for server in self.servers.iter() {
            if *server.key() == current_server {
                continue;
            }
            
            evaluated_servers += 1;
            let distance = server.value().bounds.distance_to_point(player_position);
            
            if distance <= self.transfer_distance {
                candidates_found += 1;
                match &closest_server {
                    None => {
                        closest_server = Some((*server.key(), distance));
                        debug!("First transfer candidate: '{}' at distance {:.2}", server.key(), distance);
                    }
                    Some((_, closest_distance)) => {
                        if distance < *closest_distance {
                            debug!(
                                "Better transfer candidate: '{}' at distance {:.2} (was {:.2})",
                                server.key(), distance, closest_distance
                            );
                            closest_server = Some((*server.key(), distance));
                        }
                    }
                }
            }
        }
        
        let evaluation_time = start_time.elapsed();
        
        match &closest_server {
            Some((target_server, distance)) => {
                info!(
                    "Transfer recommended: '{}' -> '{}' (distance: {:.2}, evaluated {} servers, {} candidates) in {:?}",
                    current_server, target_server, distance, evaluated_servers, candidates_found, evaluation_time
                );
            }
            None => {
                debug!(
                    "No transfer needed for server '{}' (evaluated {} servers, {} candidates) in {:?}",
                    current_server, evaluated_servers, candidates_found, evaluation_time
                );
            }
        }
        
        if evaluation_time.as_millis() > 25 {
            warn!(
                "Transfer evaluation took longer than expected: {:?} for {} servers",
                evaluation_time, server_count
            );
        }
        
        closest_server.map(|(server_id, _)| server_id)
    }
    
    pub async fn should_preload_server(&self, player_position: &Vector3, server_id: Uuid) -> bool {
        if let Some(server) = self.servers.get(&server_id) {
            let distance = server.bounds.distance_to_point(player_position);
            return distance <= self.preload_distance;
        }
        false
    }
    
    pub async fn get_nearby_servers(&self, position: &Vector3, radius: f64) -> Vec<Uuid> {
        let mut nearby = Vec::new();
        
        for server in self.servers.iter() {
            let distance = server.value().bounds.distance_to_point(position);
            if distance <= radius {
                nearby.push(*server.key());
            }
        }
        
        nearby
    }
    
    pub async fn get_server_info(&self, server_id: Uuid) -> Option<GameServerInfo> {
        self.servers.get(&server_id).map(|s| s.value().clone())
    }
    
    pub async fn get_all_servers(&self) -> Vec<GameServerInfo> {
        self.servers.iter().map(|s| s.value().clone()).collect()
    }
    
    pub async fn calculate_transfer_path(&self, from: &Vector3, to: &Vector3) -> Vec<Uuid> {
        let start_time = Instant::now();
        
        debug!(
            "Calculating transfer path from ({:.2}, {:.2}, {:.2}) to ({:.2}, {:.2}, {:.2})",
            from.x, from.y, from.z, to.x, to.y, to.z
        );
        
        let mut path = Vec::new();
        
        let from_server = self.find_server_for_position(from).await;
        let to_server = self.find_server_for_position(to).await;
        
        let migration_time = start_time.elapsed();
        
        match (&from_server, &to_server) {
            (Some(from_id), Some(to_id)) => {
                if from_id != to_id {
                    path.push(*from_id);
                    path.push(*to_id);
                    
                    let distance = from.distance_to(to);
                    info!(
                        "Transfer path calculated: '{}' -> '{}' (distance: {:.2}) in {:?}",
                        from_id, to_id, distance, migration_time
                    );
                } else {
                    debug!(
                        "No transfer needed - same server '{}' in {:?}",
                        from_id, migration_time
                    );
                }
            }
            (Some(from_id), None) => {
                warn!(
                    "Transfer path incomplete: from server '{}' found, but destination has no server in {:?}",
                    from_id, migration_time
                );
            }
            (None, Some(to_id)) => {
                warn!(
                    "Transfer path incomplete: to server '{}' found, but source has no server in {:?}",
                    to_id, migration_time
                );
            }
            (None, None) => {
                warn!(
                    "Transfer path failed: neither source nor destination positions have servers in {:?}",
                    migration_time
                );
            }
        }
        
        if migration_time.as_millis() > 30 {
            warn!(
                "Transfer path calculation took longer than expected: {:?}",
                migration_time
            );
        }
        
        debug!("Transfer path calculation completed with {} steps", path.len());
        
        path
    }
}

#[derive(Debug, Clone)]
pub struct ServerTransition {
    pub player_id: Uuid,
    pub from_server: Uuid,
    pub to_server: Uuid,
    pub initiated_at: u64,
    pub completed_at: Option<u64>,
    pub state_transferred: bool,
}

impl ServerTransition {
    pub fn new(
        player_id: Uuid,
        from_server: Uuid,
        to_server: Uuid,
    ) -> Self {
        Self {
            player_id,
            from_server,
            to_server,
            initiated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            completed_at: None,
            state_transferred: false,
        }
    }
    
    pub fn complete(&mut self) {
        self.completed_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        );
    }
    
    pub fn is_completed(&self) -> bool {
        self.completed_at.is_some()
    }
}