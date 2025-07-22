use crate::state::{GameServerInfo, ServerStatus, ServerBounds};
use crate::config::GameServerConfig;
use anyhow::{Result, anyhow};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{interval, Duration};
use tracing::{info, warn, error};
use uuid::Uuid;

pub struct GameServerManager {
    servers: Arc<DashMap<Uuid, GameServerInfo>>,  // Now keyed by UUID
    config: GameServerConfig,
}

impl GameServerManager {
    pub fn new(config: GameServerConfig) -> Self {
        Self {
            servers: Arc::new(DashMap::new()),
            config,
        }
    }
    
    pub async fn register_server(&self, mut server_info: GameServerInfo) -> Result<()> {
        server_info.status = ServerStatus::Starting;
        server_info.last_heartbeat = current_timestamp();
        
        info!("Registering game server: {}", server_info.id);
        self.servers.insert(server_info.id, server_info);
        Ok(())
    }
    
    pub async fn update_server_heartbeat(&self, server_id: Uuid) -> Result<()> {
        if let Some(mut server) = self.servers.get_mut(&server_id) {
            server.last_heartbeat = current_timestamp();
            server.status = ServerStatus::Running;
            return Ok(());
        }
        Err(anyhow!("Server not found: {}", server_id))
    }
    
    pub async fn update_player_count(&self, server_id: Uuid, count: usize) -> Result<()> {
        if let Some(mut server) = self.servers.get_mut(&server_id) {
            server.player_count = count;
            return Ok(());
        }
        Err(anyhow!("Server not found: {}", server_id))
    }
    
    pub async fn get_server(&self, server_id: Uuid) -> Option<GameServerInfo> {
        self.servers.get(&server_id).map(|s| s.value().clone())
    }
    
    // Legacy method for string-based lookups during migration
    pub async fn get_server_by_string(&self, server_id: &str) -> Option<GameServerInfo> {
        // Try to parse as UUID first
        if let Ok(uuid) = server_id.parse::<Uuid>() {
            return self.get_server(uuid).await;
        }
        
        // Fallback: search by string representation
        for server in self.servers.iter() {
            if server.value().id.to_string() == server_id {
                return Some(server.value().clone());
            }
        }
        None
    }
    
    pub async fn get_all_servers(&self) -> Vec<GameServerInfo> {
        self.servers.iter().map(|s| s.value().clone()).collect()
    }
    
    pub async fn get_servers_by_status(&self, status: ServerStatus) -> Vec<GameServerInfo> {
        self.servers
            .iter()
            .filter(|s| std::mem::discriminant(&s.value().status) == std::mem::discriminant(&status))
            .map(|s| s.value().clone())
            .collect()
    }
    
    pub async fn remove_server(&self, server_id: Uuid) -> Result<()> {
        if self.servers.remove(&server_id).is_some() {
            info!("Removed server: {}", server_id);
            Ok(())
        } else {
            Err(anyhow!("Server not found: {}", server_id))
        }
    }
    
    pub async fn start_health_monitor(&self) {
        let servers = Arc::clone(&self.servers);
        let max_idle_time = self.config.max_idle_time;
        let health_check_interval = Duration::from_secs(self.config.startup_timeout);
        
        tokio::spawn(async move {
            let mut interval = interval(health_check_interval);
            
            loop {
                interval.tick().await;
                let current_time = current_timestamp();
                
                let mut unhealthy_servers = Vec::new();
                
                for server in servers.iter() {
                    let time_since_heartbeat = current_time - server.last_heartbeat;
                    
                    if time_since_heartbeat > max_idle_time {
                        warn!(
                            "Server {} hasn't sent heartbeat for {} seconds",
                            server.id, time_since_heartbeat
                        );
                        unhealthy_servers.push(server.id);
                    }
                }
                
                for server_id in unhealthy_servers {
                    if let Some(mut server) = servers.get_mut(&server_id) {
                        server.status = ServerStatus::Failed;
                        error!("Marked server {} as failed due to missed heartbeats", server_id);
                    }
                }
            }
        });
    }
    
    // Find server by its spatial bounds
    pub async fn find_server_by_bounds(&self, server_id: Uuid) -> Option<GameServerInfo> {
        self.servers.get(&server_id)
            .filter(|s| matches!(s.value().status, ServerStatus::Running))
            .map(|s| s.value().clone())
    }
    
    pub async fn find_least_loaded_server(&self) -> Option<GameServerInfo> {
        self.servers
            .iter()
            .filter(|s| matches!(s.value().status, ServerStatus::Running))
            .min_by_key(|s| s.value().player_count)
            .map(|s| s.value().clone())
    }
    
    pub async fn get_server_capacity(&self, server_id: Uuid) -> Option<f32> {
        if let Some(server) = self.servers.get(&server_id) {
            const MAX_PLAYERS: usize = 1000;
            Some(server.player_count as f32 / MAX_PLAYERS as f32)
        } else {
            None
        }
    }
    
    pub async fn request_server_startup(&self, bounds: ServerBounds) -> Result<Uuid> {
        let server_id = Uuid::new_v4();
        let port = 7000 + (self.servers.len() % 1000) as u16;
        
        let server_info = GameServerInfo {
            id: server_id,
            address: "127.0.0.1".to_string(),
            port,
            bounds,  // Direct server bounds
            status: ServerStatus::Starting,
            player_count: 0,
            last_heartbeat: current_timestamp(),
        };
        
        self.register_server(server_info).await?;
        
        info!("Requested startup for new server: {}", server_id);
        
        Ok(server_id)
    }
    
    pub async fn request_server_shutdown(&self, server_id: Uuid) -> Result<()> {
        if let Some(mut server) = self.servers.get_mut(&server_id) {
            server.status = ServerStatus::Stopping;
            info!("Requested shutdown for server: {}", server_id);
            Ok(())
        } else {
            Err(anyhow!("Server not found: {}", server_id))
        }
    }
}

pub fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}