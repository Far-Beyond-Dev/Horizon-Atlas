use crate::state::{GameServerInfo, ServerStatus, RegionBounds};
use crate::config::GameServerConfig;
use anyhow::{Result, anyhow};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{interval, Duration};
use tracing::{info, warn, error};
use uuid::Uuid;

pub struct GameServerManager {
    servers: Arc<DashMap<String, GameServerInfo>>,
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
        self.servers.insert(server_info.id.clone(), server_info);
        Ok(())
    }
    
    pub async fn update_server_heartbeat(&self, server_id: &str) -> Result<()> {
        if let Some(mut server) = self.servers.get_mut(server_id) {
            server.last_heartbeat = current_timestamp();
            server.status = ServerStatus::Running;
            return Ok(());
        }
        Err(anyhow!("Server not found: {}", server_id))
    }
    
    pub async fn update_player_count(&self, server_id: &str, count: usize) -> Result<()> {
        if let Some(mut server) = self.servers.get_mut(server_id) {
            server.player_count = count;
            return Ok(());
        }
        Err(anyhow!("Server not found: {}", server_id))
    }
    
    pub async fn get_server(&self, server_id: &str) -> Option<GameServerInfo> {
        self.servers.get(server_id).map(|s| s.value().clone())
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
    
    pub async fn remove_server(&self, server_id: &str) -> Result<()> {
        if self.servers.remove(server_id).is_some() {
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
                        unhealthy_servers.push(server.id.clone());
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
    
    pub async fn find_server_for_region(&self, region_id: &str) -> Option<GameServerInfo> {
        for server in self.servers.iter() {
            if server.region_bounds.region_id == region_id && 
               matches!(server.status, ServerStatus::Running) {
                return Some(server.value().clone());
            }
        }
        None
    }
    
    pub async fn find_least_loaded_server(&self) -> Option<GameServerInfo> {
        self.servers
            .iter()
            .filter(|s| matches!(s.value().status, ServerStatus::Running))
            .min_by_key(|s| s.value().player_count)
            .map(|s| s.value().clone())
    }
    
    pub async fn get_server_capacity(&self, server_id: &str) -> Option<f32> {
        if let Some(server) = self.servers.get(server_id) {
            const MAX_PLAYERS: usize = 1000;
            Some(server.player_count as f32 / MAX_PLAYERS as f32)
        } else {
            None
        }
    }
    
    pub async fn request_server_startup(&self, region_bounds: RegionBounds) -> Result<String> {
        let server_id = format!("server-{}", Uuid::new_v4());
        let port = 7000 + (self.servers.len() % 1000) as u16;
        
        let server_info = GameServerInfo {
            id: server_id.clone(),
            address: "127.0.0.1".to_string(),
            port,
            region_bounds,
            status: ServerStatus::Starting,
            player_count: 0,
            last_heartbeat: current_timestamp(),
        };
        
        let region_id = server_info.region_bounds.region_id.clone();
        self.register_server(server_info).await?;
        
        info!("Requested startup for new server: {} for region: {}", 
              server_id, region_id);
        
        Ok(server_id)
    }
    
    pub async fn request_server_shutdown(&self, server_id: &str) -> Result<()> {
        if let Some(mut server) = self.servers.get_mut(server_id) {
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