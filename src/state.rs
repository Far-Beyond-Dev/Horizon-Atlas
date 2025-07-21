use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerState {
    pub id: Uuid,
    pub position: Vector3,
    pub velocity: Vector3,
    pub region_id: String,
    pub last_update: u64,
    pub data: PlayerData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vector3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerData {
    pub level: u32,
    pub health: f32,
    pub inventory: HashMap<String, u32>,
    pub custom_data: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameServerInfo {
    pub id: String,
    pub address: String,
    pub port: u16,
    pub region_bounds: RegionBounds,
    pub status: ServerStatus,
    pub player_count: usize,
    pub last_heartbeat: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionBounds {
    pub min: Vector3,
    pub max: Vector3,
    pub region_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub client_id: Uuid,
    pub player_id: Option<Uuid>,
    pub current_server: Option<String>,
    pub target_server: Option<String>,
    pub connection_time: u64,
}

impl Vector3 {
    pub fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
    
    pub fn distance_to(&self, other: &Vector3) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
}

impl RegionBounds {
    pub fn contains(&self, position: &Vector3) -> bool {
        position.x >= self.min.x && position.x <= self.max.x &&
        position.y >= self.min.y && position.y <= self.max.y &&
        position.z >= self.min.z && position.z <= self.max.z
    }
    
    pub fn distance_to_point(&self, position: &Vector3) -> f64 {
        let center = Vector3::new(
            (self.min.x + self.max.x) / 2.0,
            (self.min.y + self.max.y) / 2.0,
            (self.min.z + self.max.z) / 2.0,
        );
        center.distance_to(position)
    }
}