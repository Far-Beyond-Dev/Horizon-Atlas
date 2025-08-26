use crate::config::{RoutingConfig, LoadBalancingAlgorithm, LoadBalancingConfig};
use crate::discovery::DiscoveryManager;
use crate::errors::{AtlasError, Result};
use crate::types::{
    ClientId, ServerId, RegionId, Position, Message, MessageDestination, MessageSource,
    GameServer, ServerStatus, LoadBalancingMetrics, ClientConnection, Region
};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::{HashMap, BTreeMap};
use std::hash::{Hash, Hasher, DefaultHasher};
use std::sync::{Arc, atomic::{AtomicU32, AtomicU64, Ordering}};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{debug, info, error, warn, instrument};

#[async_trait]
pub trait LoadBalancer: Send + Sync {
    async fn select_server(&self, region_id: &RegionId, client_context: Option<&ClientConnection>) -> Result<ServerId>;
    async fn update_server_metrics(&self, server_id: &ServerId, metrics: ServerMetrics) -> Result<()>;
    async fn remove_server(&self, server_id: &ServerId) -> Result<()>;
    async fn get_server_load(&self, server_id: &ServerId) -> Result<f32>;
}

pub struct RoutingManager {
    config: RoutingConfig,
    discovery: Arc<DiscoveryManager>,
    load_balancer: Arc<dyn LoadBalancer>,
    client_sessions: Arc<DashMap<ClientId, ClientSession>>,
    region_mappings: Arc<RwLock<HashMap<RegionId, Vec<ServerId>>>>,
    server_metrics: Arc<DashMap<ServerId, ServerMetrics>>,
    routing_cache: Arc<DashMap<String, CacheEntry>>,
    metrics: RoutingMetrics,
}

#[derive(Debug, Clone)]
pub struct ClientSession {
    pub client_id: ClientId,
    pub current_server: Option<ServerId>,
    pub current_region: Option<RegionId>,
    pub position: Option<Position>,
    pub sticky_server: Option<ServerId>,
    pub last_route_time: Instant,
    pub connection_start: Instant,
}

#[derive(Debug, Clone)]
pub struct ServerMetrics {
    pub load: f32,
    pub connection_count: u32,
    pub response_time_ms: f32,
    pub error_rate: f32,
    pub last_heartbeat: Instant,
    pub status: ServerStatus,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    server_id: ServerId,
    cached_at: Instant,
    ttl: Duration,
}

#[derive(Debug, Default)]
struct RoutingMetrics {
    routes_calculated: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    load_balancer_calls: AtomicU64,
    routing_errors: AtomicU64,
}

impl RoutingManager {
    pub fn new(
        config: RoutingConfig,
        discovery: Arc<DiscoveryManager>,
    ) -> Self {
        let load_balancer = create_load_balancer(&config.load_balancing);
        
        Self {
            config,
            discovery,
            load_balancer,
            client_sessions: Arc::new(DashMap::new()),
            region_mappings: Arc::new(RwLock::new(HashMap::new())),
            server_metrics: Arc::new(DashMap::new()),
            routing_cache: Arc::new(DashMap::new()),
            metrics: RoutingMetrics::default(),
        }
    }
    
    pub async fn start(&self) -> Result<()> {
        info!("Starting routing manager");
        
        self.start_region_mapping_update_task().await;
        self.start_cache_cleanup_task().await;
        self.start_metrics_collection_task().await;
        
        Ok(())
    }
    
    #[instrument(skip(self), fields(client_id = %client_id, region_id = ?target_region))]
    pub async fn route_client(
        &self,
        client_id: &ClientId,
        target_region: Option<&RegionId>,
        position: Option<Position>,
    ) -> Result<ServerId> {
        self.metrics.routes_calculated.fetch_add(1, Ordering::SeqCst);
        
        let mut session = self.client_sessions.entry(client_id.clone()).or_insert_with(|| {
            ClientSession {
                client_id: client_id.clone(),
                current_server: None,
                current_region: None,
                position: position.clone(),
                sticky_server: None,
                last_route_time: Instant::now(),
                connection_start: Instant::now(),
            }
        });
        
        session.position = position;
        session.last_route_time = Instant::now();
        
        if let Some(region_id) = target_region {
            session.current_region = Some(region_id.clone());
            
            if self.config.session_affinity {
                if let Some(sticky_server) = &session.sticky_server {
                    if self.is_server_healthy(sticky_server).await? {
                        debug!("Using sticky server {} for client {}", sticky_server, client_id.0);
                        session.current_server = Some(sticky_server.clone());
                        return Ok(sticky_server.clone());
                    } else {
                        session.sticky_server = None;
                    }
                }
            }
            
            let cache_key = format!("{}:{}", client_id.0, region_id.0);
            if let Some(cached) = self.routing_cache.get(&cache_key) {
                if cached.cached_at.elapsed() < cached.ttl {
                    self.metrics.cache_hits.fetch_add(1, Ordering::SeqCst);
                    debug!("Cache hit for routing key {}", cache_key);
                    session.current_server = Some(cached.server_id.clone());
                    return Ok(cached.server_id.clone());
                } else {
                    self.routing_cache.remove(&cache_key);
                }
            }
            
            self.metrics.cache_misses.fetch_add(1, Ordering::SeqCst);
            
            let client_context = Some(ClientConnection {
                id: client_id.clone(),
                server_id: session.current_server.clone(),
                current_region: session.current_region.clone(),
                position: session.position.clone(),
                connected_at: Utc::now(),
                last_activity: Utc::now(),
                metadata: HashMap::new(),
            });
            
            self.metrics.load_balancer_calls.fetch_add(1, Ordering::SeqCst);
            let selected_server = self.load_balancer.select_server(region_id, client_context.as_ref()).await?;
            
            session.current_server = Some(selected_server.clone());
            
            if self.config.session_affinity && session.sticky_server.is_none() {
                session.sticky_server = Some(selected_server.clone());
            }
            
            self.routing_cache.insert(cache_key, CacheEntry {
                server_id: selected_server.clone(),
                cached_at: Instant::now(),
                ttl: Duration::from_secs(60),
            });
            
            info!("Routed client {} to server {} in region {}", 
                  client_id.0, selected_server, region_id);
            
            return Ok(selected_server);
        }
        
        Err(AtlasError::Routing("No target region specified".to_string()))
    }
    
    pub async fn route_message(&self, message: &Message) -> Result<Vec<ServerId>> {
        match &message.to {
            MessageDestination::Server(server_id) => {
                if self.is_server_healthy(server_id).await? {
                    Ok(vec![server_id.clone()])
                } else {
                    Err(AtlasError::Routing(format!("Target server {} is not healthy", server_id)))
                }
            }
            
            MessageDestination::Region(region_id) => {
                let servers = self.get_servers_for_region(region_id).await?;
                Ok(servers.into_iter()
                    .filter_map(|server| {
                        if matches!(server.status, ServerStatus::Ready) {
                            Some(server.id)
                        } else {
                            None
                        }
                    })
                    .collect())
            }
            
            MessageDestination::Client(client_id) => {
                if let Some(session) = self.client_sessions.get(client_id) {
                    if let Some(server_id) = &session.current_server {
                        Ok(vec![server_id.clone()])
                    } else {
                        Err(AtlasError::Routing(format!("Client {} has no assigned server", client_id.0)))
                    }
                } else {
                    Err(AtlasError::Routing(format!("Client {} not found", client_id.0)))
                }
            }
            
            MessageDestination::Broadcast => {
                let servers = self.discovery.get_all_servers().await;
                Ok(servers.into_iter()
                    .filter_map(|server| {
                        if matches!(server.status, ServerStatus::Ready) {
                            Some(server.id)
                        } else {
                            None
                        }
                    })
                    .collect())
            }
            
            MessageDestination::Atlas(_) => {
                Ok(vec![])
            }
        }
    }
    
    pub async fn should_transition_client(
        &self,
        client_id: &ClientId,
        new_position: &Position,
    ) -> Result<Option<RegionId>> {
        let session = self.client_sessions.get(client_id)
            .ok_or_else(|| AtlasError::Routing(format!("Client {} not found", client_id.0)))?;
        
        if let Some(current_region_id) = &session.current_region {
            let regions = self.get_all_regions().await?;
            
            let current_region = regions.iter()
                .find(|r| r.id == *current_region_id)
                .ok_or_else(|| AtlasError::RegionNotFound { region_id: current_region_id.0.clone() })?;
            
            let distance_to_current = current_region.bounds.distance_to(new_position);
            
            if distance_to_current > self.config.region_switching_threshold {
                for region in &regions {
                    if region.id != *current_region_id && region.bounds.contains(new_position) {
                        info!("Client {} should transition from region {} to region {} (distance: {:.2})",
                              client_id.0, current_region_id, region.id, distance_to_current);
                        return Ok(Some(region.id.clone()));
                    }
                }
                
                let closest_region = regions.iter()
                    .min_by(|a, b| {
                        let dist_a = a.bounds.distance_to(new_position);
                        let dist_b = b.bounds.distance_to(new_position);
                        dist_a.partial_cmp(&dist_b).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .filter(|r| r.id != *current_region_id);
                
                if let Some(closest) = closest_region {
                    let closest_distance = closest.bounds.distance_to(new_position);
                    if distance_to_current - closest_distance > self.config.region_switching_threshold * 0.5 {
                        info!("Client {} should transition to closest region {} (current distance: {:.2}, closest: {:.2})",
                              client_id.0, closest.id, distance_to_current, closest_distance);
                        return Ok(Some(closest.id.clone()));
                    }
                }
            }
        }
        
        Ok(None)
    }
    
    pub async fn update_server_metrics(&self, server_id: &ServerId, metrics: ServerMetrics) -> Result<()> {
        self.server_metrics.insert(server_id.clone(), metrics.clone());
        self.load_balancer.update_server_metrics(server_id, metrics).await
    }
    
    pub async fn remove_client(&self, client_id: &ClientId) {
        self.client_sessions.remove(client_id);
        
        let keys_to_remove: Vec<String> = self.routing_cache.iter()
            .filter_map(|entry| {
                if entry.key().starts_with(&format!("{}:", client_id.0)) {
                    Some(entry.key().clone())
                } else {
                    None
                }
            })
            .collect();
        
        for key in keys_to_remove {
            self.routing_cache.remove(&key);
        }
        
        debug!("Removed client {} from routing manager", client_id.0);
    }
    
    pub async fn remove_server(&self, server_id: &ServerId) -> Result<()> {
        self.server_metrics.remove(server_id);
        self.load_balancer.remove_server(server_id).await?;
        
        self.client_sessions.iter_mut().for_each(|mut session| {
            if session.current_server.as_ref() == Some(server_id) {
                session.current_server = None;
            }
            if session.sticky_server.as_ref() == Some(server_id) {
                session.sticky_server = None;
            }
        });
        
        let mut region_mappings = self.region_mappings.write();
        for servers in region_mappings.values_mut() {
            servers.retain(|id| id != server_id);
        }
        
        info!("Removed server {} from routing manager", server_id);
        Ok(())
    }
    
    pub fn get_routing_metrics(&self) -> RoutingMetricsSnapshot {
        RoutingMetricsSnapshot {
            routes_calculated: self.metrics.routes_calculated.load(Ordering::SeqCst),
            cache_hits: self.metrics.cache_hits.load(Ordering::SeqCst),
            cache_misses: self.metrics.cache_misses.load(Ordering::SeqCst),
            load_balancer_calls: self.metrics.load_balancer_calls.load(Ordering::SeqCst),
            routing_errors: self.metrics.routing_errors.load(Ordering::SeqCst),
            active_client_sessions: self.client_sessions.len() as u64,
            cached_routes: self.routing_cache.len() as u64,
        }
    }
    
    async fn is_server_healthy(&self, server_id: &ServerId) -> Result<bool> {
        if let Some(server) = self.discovery.get_server(server_id).await? {
            Ok(matches!(server.status, ServerStatus::Ready) && 
               server.last_heartbeat.signed_duration_since(Utc::now()).num_seconds().abs() < 30)
        } else {
            Ok(false)
        }
    }
    
    async fn get_servers_for_region(&self, region_id: &RegionId) -> Result<Vec<GameServer>> {
        self.discovery.get_servers_by_region(region_id).await
    }
    
    async fn get_all_regions(&self) -> Result<Vec<Region>> {
        Ok(vec![])
    }
    
    async fn start_region_mapping_update_task(&self) {
        let discovery = Arc::clone(&self.discovery);
        let region_mappings = Arc::clone(&self.region_mappings);
        
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));
            
            loop {
                interval.tick().await;
                
                let servers = discovery.get_all_servers().await;
                let mut new_mappings = HashMap::new();
                
                for server in servers {
                    for region_id in server.regions {
                        new_mappings.entry(region_id)
                            .or_insert_with(Vec::new)
                            .push(server.id.clone());
                    }
                }
                
                *region_mappings.write() = new_mappings;
            }
        });
    }
    
    async fn start_cache_cleanup_task(&self) {
        let routing_cache = Arc::clone(&self.routing_cache);
        
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(60));
            
            loop {
                interval.tick().await;
                
                let now = Instant::now();
                routing_cache.retain(|_, entry| {
                    now.duration_since(entry.cached_at) < entry.ttl
                });
            }
        });
    }
    
    async fn start_metrics_collection_task(&self) {
        let server_metrics = Arc::clone(&self.server_metrics);
        let discovery = Arc::clone(&self.discovery);
        
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(10));
            
            loop {
                interval.tick().await;
                
                let servers = discovery.get_all_servers().await;
                for server in servers {
                    if !server_metrics.contains_key(&server.id) {
                        let metrics = ServerMetrics {
                            load: server.load,
                            connection_count: server.current_players,
                            response_time_ms: 0.0,
                            error_rate: 0.0,
                            last_heartbeat: Instant::now(),
                            status: server.status,
                        };
                        server_metrics.insert(server.id, metrics);
                    }
                }
            }
        });
    }
}

#[derive(Debug)]
pub struct RoutingMetricsSnapshot {
    pub routes_calculated: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub load_balancer_calls: u64,
    pub routing_errors: u64,
    pub active_client_sessions: u64,
    pub cached_routes: u64,
}

fn create_load_balancer(config: &LoadBalancingConfig) -> Arc<dyn LoadBalancer> {
    match config.algorithm {
        LoadBalancingAlgorithm::RoundRobin => Arc::new(RoundRobinLoadBalancer::new()),
        LoadBalancingAlgorithm::WeightedRoundRobin => Arc::new(WeightedRoundRobinLoadBalancer::new(config.clone())),
        LoadBalancingAlgorithm::LeastConnections => Arc::new(LeastConnectionsLoadBalancer::new()),
        LoadBalancingAlgorithm::LeastResponseTime => Arc::new(LeastResponseTimeLoadBalancer::new()),
        LoadBalancingAlgorithm::ConsistentHashing => Arc::new(ConsistentHashLoadBalancer::new()),
        LoadBalancingAlgorithm::Geographic => Arc::new(GeographicLoadBalancer::new()),
    }
}

pub struct RoundRobinLoadBalancer {
    counter: AtomicU32,
}

impl RoundRobinLoadBalancer {
    fn new() -> Self {
        Self {
            counter: AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl LoadBalancer for RoundRobinLoadBalancer {
    async fn select_server(&self, region_id: &RegionId, _client_context: Option<&ClientConnection>) -> Result<ServerId> {
        let servers = vec![];
        if servers.is_empty() {
            return Err(AtlasError::ServerNotFound { server_id: "none".to_string() });
        }
        
        let index = self.counter.fetch_add(1, Ordering::SeqCst) as usize % servers.len();
        Ok(servers[index].clone())
    }
    
    async fn update_server_metrics(&self, _server_id: &ServerId, _metrics: ServerMetrics) -> Result<()> {
        Ok(())
    }
    
    async fn remove_server(&self, _server_id: &ServerId) -> Result<()> {
        Ok(())
    }
    
    async fn get_server_load(&self, _server_id: &ServerId) -> Result<f32> {
        Ok(0.0)
    }
}

pub struct WeightedRoundRobinLoadBalancer {
    config: LoadBalancingConfig,
    server_weights: Arc<DashMap<ServerId, f32>>,
    current_weights: Arc<Mutex<HashMap<ServerId, f32>>>,
}

impl WeightedRoundRobinLoadBalancer {
    fn new(config: LoadBalancingConfig) -> Self {
        Self {
            config,
            server_weights: Arc::new(DashMap::new()),
            current_weights: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl LoadBalancer for WeightedRoundRobinLoadBalancer {
    async fn select_server(&self, _region_id: &RegionId, _client_context: Option<&ClientConnection>) -> Result<ServerId> {
        let mut current_weights = self.current_weights.lock().await;
        
        if current_weights.is_empty() {
            return Err(AtlasError::ServerNotFound { server_id: "none".to_string() });
        }
        
        let mut selected = None;
        let mut max_weight = f32::MIN;
        
        for (server_id, weight) in current_weights.iter_mut() {
            if let Some(base_weight) = self.server_weights.get(server_id) {
                *weight += *base_weight;
                if *weight > max_weight {
                    max_weight = *weight;
                    selected = Some(server_id.clone());
                }
            }
        }
        
        if let Some(selected_server) = selected {
            let total_weight: f32 = self.server_weights.iter().map(|entry| *entry.value()).sum();
            if let Some(weight) = current_weights.get_mut(&selected_server) {
                *weight -= total_weight;
            }
            Ok(selected_server)
        } else {
            Err(AtlasError::ServerNotFound { server_id: "none".to_string() })
        }
    }
    
    async fn update_server_metrics(&self, server_id: &ServerId, metrics: ServerMetrics) -> Result<()> {
        let weight = self.calculate_weight(&metrics);
        self.server_weights.insert(server_id.clone(), weight);
        
        let mut current_weights = self.current_weights.lock().await;
        current_weights.entry(server_id.clone()).or_insert(0.0);
        
        Ok(())
    }
    
    async fn remove_server(&self, server_id: &ServerId) -> Result<()> {
        self.server_weights.remove(server_id);
        let mut current_weights = self.current_weights.lock().await;
        current_weights.remove(server_id);
        Ok(())
    }
    
    async fn get_server_load(&self, server_id: &ServerId) -> Result<f32> {
        Ok(self.server_weights.get(server_id).map(|w| *w).unwrap_or(0.0))
    }
}

impl WeightedRoundRobinLoadBalancer {
    fn calculate_weight(&self, metrics: &ServerMetrics) -> f32 {
        let health_score = if matches!(metrics.status, ServerStatus::Ready) { 1.0 } else { 0.1 };
        let load_score = 1.0 - metrics.load.min(1.0);
        let latency_score = 1.0 / (1.0 + metrics.response_time_ms / 1000.0);
        let connection_score = 1.0 / (1.0 + metrics.connection_count as f32 / 1000.0);
        
        (health_score * self.config.health_check_weight +
         load_score * self.config.load_weight +
         latency_score * self.config.latency_weight +
         connection_score * self.config.connection_weight).max(0.01)
    }
}

pub struct LeastConnectionsLoadBalancer {
    server_connections: Arc<DashMap<ServerId, u32>>,
}

impl LeastConnectionsLoadBalancer {
    fn new() -> Self {
        Self {
            server_connections: Arc::new(DashMap::new()),
        }
    }
}

#[async_trait]
impl LoadBalancer for LeastConnectionsLoadBalancer {
    async fn select_server(&self, _region_id: &RegionId, _client_context: Option<&ClientConnection>) -> Result<ServerId> {
        let min_connections = self.server_connections.iter()
            .min_by_key(|entry| *entry.value())
            .map(|entry| (entry.key().clone(), *entry.value()));
        
        if let Some((server_id, _)) = min_connections {
            Ok(server_id)
        } else {
            Err(AtlasError::ServerNotFound { server_id: "none".to_string() })
        }
    }
    
    async fn update_server_metrics(&self, server_id: &ServerId, metrics: ServerMetrics) -> Result<()> {
        self.server_connections.insert(server_id.clone(), metrics.connection_count);
        Ok(())
    }
    
    async fn remove_server(&self, server_id: &ServerId) -> Result<()> {
        self.server_connections.remove(server_id);
        Ok(())
    }
    
    async fn get_server_load(&self, server_id: &ServerId) -> Result<f32> {
        Ok(self.server_connections.get(server_id).map(|c| *c as f32 / 1000.0).unwrap_or(0.0))
    }
}

pub struct LeastResponseTimeLoadBalancer {
    server_response_times: Arc<DashMap<ServerId, f32>>,
}

impl LeastResponseTimeLoadBalancer {
    fn new() -> Self {
        Self {
            server_response_times: Arc::new(DashMap::new()),
        }
    }
}

#[async_trait]
impl LoadBalancer for LeastResponseTimeLoadBalancer {
    async fn select_server(&self, _region_id: &RegionId, _client_context: Option<&ClientConnection>) -> Result<ServerId> {
        let min_response_time = self.server_response_times.iter()
            .min_by(|a, b| a.value().partial_cmp(b.value()).unwrap_or(std::cmp::Ordering::Equal))
            .map(|entry| entry.key().clone());
        
        min_response_time.ok_or_else(|| AtlasError::ServerNotFound { server_id: "none".to_string() })
    }
    
    async fn update_server_metrics(&self, server_id: &ServerId, metrics: ServerMetrics) -> Result<()> {
        self.server_response_times.insert(server_id.clone(), metrics.response_time_ms);
        Ok(())
    }
    
    async fn remove_server(&self, server_id: &ServerId) -> Result<()> {
        self.server_response_times.remove(server_id);
        Ok(())
    }
    
    async fn get_server_load(&self, server_id: &ServerId) -> Result<f32> {
        Ok(self.server_response_times.get(server_id).map(|t| *t / 1000.0).unwrap_or(0.0))
    }
}

pub struct ConsistentHashLoadBalancer {
    hash_ring: Arc<RwLock<BTreeMap<u64, ServerId>>>,
    virtual_nodes: u32,
}

impl ConsistentHashLoadBalancer {
    fn new() -> Self {
        Self {
            hash_ring: Arc::new(RwLock::new(BTreeMap::new())),
            virtual_nodes: 150,
        }
    }
    
    fn hash_key(&self, key: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }
}

#[async_trait]
impl LoadBalancer for ConsistentHashLoadBalancer {
    async fn select_server(&self, _region_id: &RegionId, client_context: Option<&ClientConnection>) -> Result<ServerId> {
        let client_key = client_context
            .map(|c| c.id.0.clone())
            .unwrap_or_else(|| "default".to_string());
        
        let hash = self.hash_key(&client_key);
        let ring = self.hash_ring.read();
        
        let server_id = ring.range(hash..)
            .next()
            .or_else(|| ring.iter().next())
            .map(|(_, server_id)| server_id.clone());
        
        server_id.ok_or_else(|| AtlasError::ServerNotFound { server_id: "none".to_string() })
    }
    
    async fn update_server_metrics(&self, server_id: &ServerId, _metrics: ServerMetrics) -> Result<()> {
        let mut ring = self.hash_ring.write();
        
        for i in 0..self.virtual_nodes {
            let virtual_key = format!("{}:{}", server_id.0, i);
            let hash = self.hash_key(&virtual_key);
            ring.insert(hash, server_id.clone());
        }
        
        Ok(())
    }
    
    async fn remove_server(&self, server_id: &ServerId) -> Result<()> {
        let mut ring = self.hash_ring.write();
        ring.retain(|_, id| id != server_id);
        Ok(())
    }
    
    async fn get_server_load(&self, _server_id: &ServerId) -> Result<f32> {
        Ok(0.0)
    }
}

pub struct GeographicLoadBalancer {
    server_locations: Arc<DashMap<ServerId, Position>>,
}

impl GeographicLoadBalancer {
    fn new() -> Self {
        Self {
            server_locations: Arc::new(DashMap::new()),
        }
    }
    
    fn calculate_distance(&self, pos1: &Position, pos2: &Position) -> f64 {
        let dx = pos1.x - pos2.x;
        let dy = pos1.y - pos2.y;
        let dz = pos1.z - pos2.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
}

#[async_trait]
impl LoadBalancer for GeographicLoadBalancer {
    async fn select_server(&self, _region_id: &RegionId, client_context: Option<&ClientConnection>) -> Result<ServerId> {
        let client_position = client_context
            .and_then(|c| c.position.as_ref());
        
        if let Some(client_pos) = client_position {
            let closest_server = self.server_locations.iter()
                .min_by(|a, b| {
                    let dist_a = self.calculate_distance(client_pos, a.value());
                    let dist_b = self.calculate_distance(client_pos, b.value());
                    dist_a.partial_cmp(&dist_b).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|entry| entry.key().clone());
            
            closest_server.ok_or_else(|| AtlasError::ServerNotFound { server_id: "none".to_string() })
        } else {
            self.server_locations.iter()
                .next()
                .map(|entry| entry.key().clone())
                .ok_or_else(|| AtlasError::ServerNotFound { server_id: "none".to_string() })
        }
    }
    
    async fn update_server_metrics(&self, server_id: &ServerId, _metrics: ServerMetrics) -> Result<()> {
        if !self.server_locations.contains_key(server_id) {
            self.server_locations.insert(server_id.clone(), Position { x: 0.0, y: 0.0, z: 0.0 });
        }
        Ok(())
    }
    
    async fn remove_server(&self, server_id: &ServerId) -> Result<()> {
        self.server_locations.remove(server_id);
        Ok(())
    }
    
    async fn get_server_load(&self, _server_id: &ServerId) -> Result<f32> {
        Ok(0.0)
    }
}