use crate::state::{Vector3, RegionBounds};
use anyhow::Result;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct RegionManager {
    regions: Arc<DashMap<String, RegionBounds>>,
    server_assignments: Arc<DashMap<String, String>>,
    player_positions: Arc<RwLock<HashMap<Uuid, Vector3>>>,
    preload_distance: f64,
    transfer_distance: f64,
}

impl RegionManager {
    pub fn new(preload_distance: f64, transfer_distance: f64) -> Self {
        Self {
            regions: Arc::new(DashMap::new()),
            server_assignments: Arc::new(DashMap::new()),
            player_positions: Arc::new(RwLock::new(HashMap::new())),
            preload_distance,
            transfer_distance,
        }
    }
    
    pub async fn register_region(&self, bounds: RegionBounds, server_id: String) -> Result<()> {
        let region_id = bounds.region_id.clone();
        self.regions.insert(region_id.clone(), bounds);
        self.server_assignments.insert(server_id, region_id);
        Ok(())
    }
    
    pub async fn unregister_region(&self, region_id: &str) -> Result<()> {
        self.regions.remove(region_id);
        self.server_assignments.retain(|_, v| v != region_id);
        Ok(())
    }
    
    pub async fn update_player_position(&self, player_id: Uuid, position: Vector3) -> Result<()> {
        let mut positions = self.player_positions.write().await;
        positions.insert(player_id, position);
        Ok(())
    }
    
    pub async fn find_region_for_position(&self, position: &Vector3) -> Option<String> {
        for region in self.regions.iter() {
            if region.value().contains(position) {
                return Some(region.key().clone());
            }
        }
        None
    }
    
    pub async fn find_server_for_position(&self, position: &Vector3) -> Option<String> {
        let region_id = self.find_region_for_position(position).await?;
        
        for assignment in self.server_assignments.iter() {
            if assignment.value() == &region_id {
                return Some(assignment.key().clone());
            }
        }
        None
    }
    
    pub async fn should_preload_region(&self, player_position: &Vector3, region_id: &str) -> bool {
        if let Some(region) = self.regions.get(region_id) {
            let distance = region.distance_to_point(player_position);
            return distance <= self.preload_distance;
        }
        false
    }
    
    pub async fn should_transfer_player(&self, player_position: &Vector3, current_region: &str) -> Option<String> {
        let mut closest_region: Option<(String, f64)> = None;
        
        for region in self.regions.iter() {
            if region.key() == current_region {
                continue;
            }
            
            let distance = region.value().distance_to_point(player_position);
            if distance <= self.transfer_distance {
                match &closest_region {
                    None => closest_region = Some((region.key().clone(), distance)),
                    Some((_, closest_distance)) => {
                        if distance < *closest_distance {
                            closest_region = Some((region.key().clone(), distance));
                        }
                    }
                }
            }
        }
        
        closest_region.map(|(region_id, _)| region_id)
    }
    
    pub async fn get_nearby_regions(&self, position: &Vector3, radius: f64) -> Vec<String> {
        let mut nearby = Vec::new();
        
        for region in self.regions.iter() {
            let distance = region.value().distance_to_point(position);
            if distance <= radius {
                nearby.push(region.key().clone());
            }
        }
        
        nearby
    }
    
    pub async fn get_region_bounds(&self, region_id: &str) -> Option<RegionBounds> {
        self.regions.get(region_id).map(|r| r.value().clone())
    }
    
    pub async fn get_all_regions(&self) -> Vec<RegionBounds> {
        self.regions.iter().map(|r| r.value().clone()).collect()
    }
    
    pub async fn calculate_migration_path(&self, from: &Vector3, to: &Vector3) -> Vec<String> {
        let mut path = Vec::new();
        
        let from_region = self.find_region_for_position(from).await;
        let to_region = self.find_region_for_position(to).await;
        
        if let (Some(from_id), Some(to_id)) = (from_region, to_region) {
            if from_id != to_id {
                path.push(from_id);
                path.push(to_id);
            }
        }
        
        path
    }
}

#[derive(Debug, Clone)]
pub struct RegionTransition {
    pub player_id: Uuid,
    pub from_region: String,
    pub to_region: String,
    pub from_server: String,
    pub to_server: String,
    pub initiated_at: u64,
    pub completed_at: Option<u64>,
    pub state_transferred: bool,
}

impl RegionTransition {
    pub fn new(
        player_id: Uuid,
        from_region: String,
        to_region: String,
        from_server: String,
        to_server: String,
    ) -> Self {
        Self {
            player_id,
            from_region,
            to_region,
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