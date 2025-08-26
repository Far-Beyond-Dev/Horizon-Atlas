use crate::config::{DiscoveryConfig, DiscoveryBackend, StaticServer};
use crate::errors::{AtlasError, Result};
use crate::types::{GameServer, ServerId, ServerStatus, Position};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

#[async_trait]
pub trait ServiceDiscovery: Send + Sync {
    async fn register_server(&self, server: &GameServer) -> Result<()>;
    async fn deregister_server(&self, server_id: &ServerId) -> Result<()>;
    async fn discover_servers(&self) -> Result<Vec<GameServer>>;
    async fn get_server(&self, server_id: &ServerId) -> Result<Option<GameServer>>;
    async fn update_server_status(&self, server_id: &ServerId, status: ServerStatus) -> Result<()>;
    async fn health_check(&self) -> Result<bool>;
}

pub struct DiscoveryManager {
    backend: Arc<dyn ServiceDiscovery>,
    config: DiscoveryConfig,
    server_cache: Arc<DashMap<ServerId, GameServer>>,
    discovery_task: Option<tokio::task::JoinHandle<()>>,
}

impl DiscoveryManager {
    pub async fn new(config: DiscoveryConfig) -> Result<Self> {
        let backend = create_discovery_backend(&config).await?;
        
        Ok(Self {
            backend,
            config,
            server_cache: Arc::new(DashMap::new()),
            discovery_task: None,
        })
    }
    
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting discovery manager");
        
        let backend = Arc::clone(&self.backend);
        let server_cache = Arc::clone(&self.server_cache);
        let interval_ms = self.config.discovery_interval;
        
        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(interval_ms));
            
            loop {
                interval.tick().await;
                
                match backend.discover_servers().await {
                    Ok(servers) => {
                        debug!("Discovered {} servers", servers.len());
                        update_server_cache(&server_cache, servers).await;
                    }
                    Err(e) => {
                        error!("Failed to discover servers: {}", e);
                    }
                }
            }
        });
        
        self.discovery_task = Some(task);
        Ok(())
    }
    
    pub async fn stop(&mut self) {
        if let Some(task) = self.discovery_task.take() {
            task.abort();
        }
        info!("Discovery manager stopped");
    }
    
    pub async fn register_server(&self, server: &GameServer) -> Result<()> {
        self.backend.register_server(server).await?;
        
        self.server_cache.insert(server.id.clone(), server.clone());
        
        info!("Registered server {} with gameworld bounds {:?}", server.id, server.gameworld_bounds);
        Ok(())
    }
    
    pub async fn deregister_server(&self, server_id: &ServerId) -> Result<()> {
        self.server_cache.remove(server_id);
        
        self.backend.deregister_server(server_id).await?;
        info!("Deregistered server {}", server_id);
        Ok(())
    }
    
    pub async fn get_server(&self, server_id: &ServerId) -> Result<Option<GameServer>> {
        if let Some(server) = self.server_cache.get(server_id) {
            return Ok(Some(server.clone()));
        }
        
        self.backend.get_server(server_id).await
    }
    
    pub async fn get_server_for_position(&self, position: &Position) -> Result<Option<GameServer>> {
        for server_entry in self.server_cache.iter() {
            let server = server_entry.value();
            if server.gameworld_bounds.contains(position) {
                return Ok(Some(server.clone()));
            }
        }
        Ok(None)
    }
    
    pub async fn get_all_servers(&self) -> Vec<GameServer> {
        self.server_cache.iter().map(|entry| entry.value().clone()).collect()
    }
    
    pub async fn update_server_status(&self, server_id: &ServerId, status: ServerStatus) -> Result<()> {
        if let Some(mut server) = self.server_cache.get_mut(server_id) {
            server.status = status.clone();
        }
        
        self.backend.update_server_status(server_id, status).await
    }
    
    pub async fn health_check(&self) -> Result<bool> {
        self.backend.health_check().await
    }
}

async fn update_server_cache(
    cache: &DashMap<ServerId, GameServer>,
    servers: Vec<GameServer>,
) {
    cache.clear();
    
    for server in servers {
        cache.insert(server.id.clone(), server.clone());
    }
}

async fn create_discovery_backend(config: &DiscoveryConfig) -> Result<Arc<dyn ServiceDiscovery>> {
    match &config.backend {
        DiscoveryBackend::Etcd { .. } => {
            warn!("Etcd backend not available in this build - falling back to static discovery");
            Ok(Arc::new(StaticDiscovery::new(vec![])))
        }
        DiscoveryBackend::Consul { endpoint } => {
            Ok(Arc::new(ConsulDiscovery::new(endpoint.clone(), config.clone()).await?))
        }
        DiscoveryBackend::Static { servers } => {
            Ok(Arc::new(StaticDiscovery::new(servers.clone())))
        }
    }
}

// EtcdDiscovery implementation removed - requires protobuf compiler
// Uncomment and implement when protobuf is available

pub struct ConsulDiscovery {
    client: reqwest::Client,
    endpoint: String,
    config: DiscoveryConfig,
}

impl ConsulDiscovery {
    pub async fn new(endpoint: String, config: DiscoveryConfig) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
            endpoint,
            config,
        })
    }
}

#[async_trait]
impl ServiceDiscovery for ConsulDiscovery {
    async fn register_server(&self, server: &GameServer) -> Result<()> {
        let service = serde_json::json!({
            "ID": server.id.0,
            "Name": self.config.service_name,
            "Address": server.address.ip().to_string(),
            "Port": server.address.port(),
            "Meta": {
                "gameworld_bounds": serde_json::to_string(&server.gameworld_bounds)?,
                "status": serde_json::to_string(&server.status)?,
                "load": server.load.to_string(),
                "capacity": server.current_players.to_string(),
                "max_capacity": server.max_capacity.to_string(),
            },
            "Check": {
                "HTTP": format!("http://{}:{}/health", server.address.ip(), server.address.port()),
                "Interval": format!("{}ms", self.config.health_check_interval),
                "Timeout": "5s",
            }
        });
        
        let url = format!("{}/v1/agent/service/register", self.endpoint);
        let resp = self.client.put(&url).json(&service).send().await?;
        
        if !resp.status().is_success() {
            return Err(AtlasError::Discovery(
                format!("Failed to register server: {}", resp.status())
            ));
        }
        
        Ok(())
    }
    
    async fn deregister_server(&self, server_id: &ServerId) -> Result<()> {
        let url = format!("{}/v1/agent/service/deregister/{}", self.endpoint, server_id.0);
        let resp = self.client.put(&url).send().await?;
        
        if !resp.status().is_success() {
            return Err(AtlasError::Discovery(
                format!("Failed to deregister server: {}", resp.status())
            ));
        }
        
        Ok(())
    }
    
    async fn discover_servers(&self) -> Result<Vec<GameServer>> {
        let url = format!("{}/v1/health/service/{}", self.endpoint, self.config.service_name);
        let resp = self.client.get(&url).query(&[("passing", "true")]).send().await?;
        
        if !resp.status().is_success() {
            return Err(AtlasError::Discovery(
                format!("Failed to discover servers: {}", resp.status())
            ));
        }
        
        let services: serde_json::Value = resp.json().await?;
        let mut servers = Vec::new();
        
        if let Some(services_array) = services.as_array() {
            for service in services_array {
                if let Some(service_info) = service.get("Service") {
                    match self.parse_consul_service(service_info) {
                        Ok(server) => servers.push(server),
                        Err(e) => warn!("Failed to parse service: {}", e),
                    }
                }
            }
        }
        
        Ok(servers)
    }
    
    async fn get_server(&self, server_id: &ServerId) -> Result<Option<GameServer>> {
        let servers = self.discover_servers().await?;
        Ok(servers.into_iter().find(|s| s.id == *server_id))
    }
    
    
    async fn update_server_status(&self, server_id: &ServerId, status: ServerStatus) -> Result<()> {
        if let Some(server) = self.get_server(server_id).await? {
            let mut updated_server = server;
            updated_server.status = status;
            self.register_server(&updated_server).await?;
        }
        Ok(())
    }
    
    async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/v1/agent/self", self.endpoint);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }
}

impl ConsulDiscovery {
    fn parse_consul_service(&self, service: &serde_json::Value) -> Result<GameServer> {
        let id = service.get("ID")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AtlasError::Discovery("Missing service ID".to_string()))?;
        
        let address = service.get("Address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AtlasError::Discovery("Missing service address".to_string()))?;
        
        let port = service.get("Port")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| AtlasError::Discovery("Missing service port".to_string()))?;
        
        let meta = service.get("Meta").unwrap_or(&serde_json::Value::Null);
        
        use crate::types::RegionBounds;
        
        let gameworld_bounds: RegionBounds = meta.get("gameworld_bounds")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(RegionBounds {
                min_x: 0.0, max_x: 1000.0,
                min_y: 0.0, max_y: 1000.0, 
                min_z: 0.0, max_z: 1000.0,
            });
        
        let status: ServerStatus = meta.get("status")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(ServerStatus::Ready);
        
        let load: f32 = meta.get("load")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        
        let current_players: u32 = meta.get("capacity")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        
        let max_capacity: u32 = meta.get("max_capacity")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);
        
        Ok(GameServer {
            id: ServerId::from_string(id.to_string()),
            address: format!("{}:{}", address, port).parse()
                .map_err(|e| AtlasError::Discovery(format!("Invalid address: {}", e)))?,
            gameworld_bounds,
            status,
            load,
            max_capacity,
            current_players,
            last_heartbeat: Utc::now(),
            metadata: HashMap::new(),
        })
    }
}

pub struct StaticDiscovery {
    servers: Arc<RwLock<Vec<GameServer>>>,
}

impl StaticDiscovery {
    pub fn new(static_servers: Vec<StaticServer>) -> Self {
        let servers = static_servers.into_iter()
            .map(|s| GameServer {
                id: ServerId::from_string(s.id),
                address: s.address,
                gameworld_bounds: s.gameworld_bounds,
                status: ServerStatus::Ready,
                load: 0.0,
                max_capacity: 1000,
                current_players: 0,
                last_heartbeat: Utc::now(),
                metadata: HashMap::new(),
            })
            .collect();
        
        Self {
            servers: Arc::new(RwLock::new(servers)),
        }
    }
}

#[async_trait]
impl ServiceDiscovery for StaticDiscovery {
    async fn register_server(&self, server: &GameServer) -> Result<()> {
        let mut servers = self.servers.write().await;
        if let Some(pos) = servers.iter().position(|s| s.id == server.id) {
            servers[pos] = server.clone();
        } else {
            servers.push(server.clone());
        }
        Ok(())
    }
    
    async fn deregister_server(&self, server_id: &ServerId) -> Result<()> {
        let mut servers = self.servers.write().await;
        servers.retain(|s| s.id != *server_id);
        Ok(())
    }
    
    async fn discover_servers(&self) -> Result<Vec<GameServer>> {
        let servers = self.servers.read().await;
        Ok(servers.clone())
    }
    
    async fn get_server(&self, server_id: &ServerId) -> Result<Option<GameServer>> {
        let servers = self.servers.read().await;
        Ok(servers.iter().find(|s| s.id == *server_id).cloned())
    }
    
    
    async fn update_server_status(&self, server_id: &ServerId, status: ServerStatus) -> Result<()> {
        let mut servers = self.servers.write().await;
        if let Some(server) = servers.iter_mut().find(|s| s.id == *server_id) {
            server.status = status;
        }
        Ok(())
    }
    
    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }
}