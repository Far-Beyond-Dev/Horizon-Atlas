use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use uuid::Uuid;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ServerId(pub String);

impl ServerId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
    
    pub fn from_string(id: String) -> Self {
        Self(id)
    }
}

impl Default for ServerId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ServerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ClientId(pub String);

impl ClientId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for ClientId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RegionId(pub String);

impl RegionId {
    pub fn new(id: String) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for RegionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub id: RegionId,
    pub bounds: RegionBounds,
    pub server_id: Option<ServerId>,
    pub load: f32,
    pub max_capacity: u32,
    pub current_players: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionBounds {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
    pub min_z: f64,
    pub max_z: f64,
}

impl RegionBounds {
    pub fn contains(&self, pos: &Position) -> bool {
        pos.x >= self.min_x && pos.x <= self.max_x &&
        pos.y >= self.min_y && pos.y <= self.max_y &&
        pos.z >= self.min_z && pos.z <= self.max_z
    }
    
    pub fn distance_to(&self, pos: &Position) -> f64 {
        let dx = if pos.x < self.min_x {
            self.min_x - pos.x
        } else if pos.x > self.max_x {
            pos.x - self.max_x
        } else {
            0.0
        };
        
        let dy = if pos.y < self.min_y {
            self.min_y - pos.y
        } else if pos.y > self.max_y {
            pos.y - self.max_y
        } else {
            0.0
        };
        
        let dz = if pos.z < self.min_z {
            self.min_z - pos.z
        } else if pos.z > self.max_z {
            pos.z - self.max_z
        } else {
            0.0
        };
        
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameServer {
    pub id: ServerId,
    pub address: SocketAddr,
    pub regions: Vec<RegionId>,
    pub status: ServerStatus,
    pub load: f32,
    pub max_capacity: u32,
    pub current_players: u32,
    pub last_heartbeat: DateTime<Utc>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ServerStatus {
    Starting,
    Ready,
    Overloaded,
    Draining,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConnection {
    pub id: ClientId,
    pub server_id: Option<ServerId>,
    pub current_region: Option<RegionId>,
    pub position: Option<Position>,
    pub connected_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub from: MessageSource,
    pub to: MessageDestination,
    pub message_type: String,
    pub payload: Vec<u8>,
    pub timestamp: DateTime<Utc>,
    pub encrypted: bool,
    pub compressed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageSource {
    Client(ClientId),
    Server(ServerId),
    Atlas(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageDestination {
    Client(ClientId),
    Server(ServerId),
    Region(RegionId),
    Broadcast,
    Atlas(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionRequest {
    pub client_id: ClientId,
    pub from_server: ServerId,
    pub to_server: ServerId,
    pub from_region: RegionId,
    pub to_region: RegionId,
    pub position: Position,
    pub state_data: Vec<u8>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterNode {
    pub id: String,
    pub address: SocketAddr,
    pub role: NodeRole,
    pub status: NodeStatus,
    pub last_seen: DateTime<Utc>,
    pub load: f32,
    pub regions_managed: Vec<RegionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeRole {
    Leader,
    Follower,
    Candidate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeStatus {
    Active,
    Inactive,
    Failed,
    Starting,
    Stopping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub node_id: String,
    pub timestamp: DateTime<Utc>,
    pub status: NodeStatus,
    pub cpu_usage: f32,
    pub memory_usage: f32,
    pub network_usage: f32,
    pub active_connections: u32,
    pub message_throughput: f32,
    pub error_rate: f32,
    pub latency_p95: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancingMetrics {
    pub region_id: RegionId,
    pub server_loads: HashMap<ServerId, f32>,
    pub connection_counts: HashMap<ServerId, u32>,
    pub response_times: HashMap<ServerId, f32>,
    pub error_rates: HashMap<ServerId, f32>,
    pub last_updated: DateTime<Utc>,
}