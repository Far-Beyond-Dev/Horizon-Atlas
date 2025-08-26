use crate::config::RoutingConfig;
use crate::discovery::DiscoveryManager;
use crate::errors::{AtlasError, Result};
use crate::types::{
    ClientId, ServerId, Position, Message, MessageDestination, 
    GameServer, ServerStatus, ClientConnection
};
use serde::{Serialize, Deserialize};
use chrono::Utc;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::{Duration, Instant};
use tracing::{debug, info, error, warn, instrument};

pub struct RoutingManager {
    config: RoutingConfig,
    discovery: Arc<DiscoveryManager>,
    client_sessions: Arc<DashMap<ClientId, ClientSession>>,
    routing_cache: Arc<DashMap<String, CacheEntry>>,
    metrics: RoutingMetrics,
}

#[derive(Debug, Clone)]
pub struct ClientSession {
    pub client_id: ClientId,
    pub current_server: Option<ServerId>,
    pub position: Option<Position>,
    pub sticky_server: Option<ServerId>,
    pub last_route_time: Instant,
    pub connection_start: Instant,
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
    position_lookups: AtomicU64,
    routing_errors: AtomicU64,
}

impl RoutingManager {
    pub fn new(
        config: RoutingConfig,
        discovery: Arc<DiscoveryManager>,
    ) -> Self {
        Self {
            config,
            discovery,
            client_sessions: Arc::new(DashMap::new()),
            routing_cache: Arc::new(DashMap::new()),
            metrics: RoutingMetrics::default(),
        }
    }
    
    pub async fn start(&self) -> Result<()> {
        info!("Starting position-based routing manager");
        
        self.start_cache_cleanup_task().await;
        
        Ok(())
    }
    
    #[instrument(skip(self), fields(client_id = %client_id))]
    pub async fn route_client(
        &self,
        client_id: &ClientId,
        position: &Position,
    ) -> Result<ServerId> {
        self.metrics.routes_calculated.fetch_add(1, Ordering::SeqCst);
        
        let mut session = self.client_sessions.entry(client_id.clone()).or_insert_with(|| {
            ClientSession {
                client_id: client_id.clone(),
                current_server: None,
                position: Some(position.clone()),
                sticky_server: None,
                last_route_time: Instant::now(),
                connection_start: Instant::now(),
            }
        });
        
        session.position = Some(position.clone());
        session.last_route_time = Instant::now();
        
        // Check for sticky server if session affinity is enabled
        if self.config.session_affinity {
            if let Some(sticky_server) = session.sticky_server.clone() {
                if self.is_server_healthy(&sticky_server).await? {
                    if let Some(server) = self.discovery.get_server(&sticky_server).await? {
                        if server.gameworld_bounds.contains(position) {
                            debug!("Using sticky server {} for client {} at position {:?}", 
                                   sticky_server, client_id.0, position);
                            session.current_server = Some(sticky_server.clone());
                            return Ok(sticky_server);
                        } else {
                            // Position moved outside sticky server bounds, clear it
                            session.sticky_server = None;
                        }
                    }
                } else {
                    session.sticky_server = None;
                }
            }
        }
        
        // Check cache
        let cache_key = format!("{}:{}:{}:{}", client_id.0, position.x as i32, position.y as i32, position.z as i32);
        if let Some(cached) = self.routing_cache.get(&cache_key) {
            if cached.cached_at.elapsed() < cached.ttl {
                self.metrics.cache_hits.fetch_add(1, Ordering::SeqCst);
                debug!("Cache hit for position routing key {}", cache_key);
                session.current_server = Some(cached.server_id.clone());
                return Ok(cached.server_id.clone());
            } else {
                self.routing_cache.remove(&cache_key);
            }
        }
        
        self.metrics.cache_misses.fetch_add(1, Ordering::SeqCst);
        self.metrics.position_lookups.fetch_add(1, Ordering::SeqCst);
        
        // Find server that owns this gameworld position
        let selected_server = self.find_server_for_position(position).await?;
        
        session.current_server = Some(selected_server.clone());
        
        // Set sticky server for session affinity
        if self.config.session_affinity && session.sticky_server.is_none() {
            session.sticky_server = Some(selected_server.clone());
        }
        
        // Cache the result
        self.routing_cache.insert(cache_key, CacheEntry {
            server_id: selected_server.clone(),
            cached_at: Instant::now(),
            ttl: Duration::from_secs(30), // Shorter TTL since positions change frequently
        });
        
        info!("Routed client {} to server {} for position {:?}", 
              client_id.0, selected_server, position);
        
        Ok(selected_server)
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
            
            MessageDestination::Region(_) => {
                // Regions don't make sense in position-based routing
                Err(AtlasError::Routing("Region-based routing not supported in position-based system".to_string()))
            }
        }
    }
    
    pub async fn should_transition_client(
        &self,
        client_id: &ClientId,
        new_position: &Position,
    ) -> Result<Option<ServerId>> {
        let session = self.client_sessions.get(client_id)
            .ok_or_else(|| AtlasError::Routing(format!("Client {} not found", client_id.0)))?;
        
        if let Some(current_server_id) = &session.current_server {
            // Check if current server still owns this position
            if let Some(current_server) = self.discovery.get_server(current_server_id).await? {
                if current_server.gameworld_bounds.contains(new_position) {
                    // Still within current server's bounds
                    return Ok(None);
                }
            }
            
            // Need to transition - find new server
            match self.find_server_for_position(new_position).await {
                Ok(new_server_id) => {
                    if new_server_id != *current_server_id {
                        info!("Client {} should transition from server {} to server {} at position {:?}",
                              client_id.0, current_server_id, new_server_id, new_position);
                        return Ok(Some(new_server_id));
                    }
                }
                Err(e) => {
                    warn!("Failed to find server for position {:?}: {}", new_position, e);
                    return Err(e);
                }
            }
        }
        
        Ok(None)
    }
    
    pub async fn remove_client(&self, client_id: &ClientId) {
        self.client_sessions.remove(client_id);
        
        // Remove cached routes for this client
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
        // Clear server from any client sessions
        self.client_sessions.iter_mut().for_each(|mut session| {
            if session.current_server.as_ref() == Some(server_id) {
                session.current_server = None;
            }
            if session.sticky_server.as_ref() == Some(server_id) {
                session.sticky_server = None;
            }
        });
        
        info!("Removed server {} from routing manager", server_id);
        Ok(())
    }
    
    pub fn get_routing_metrics(&self) -> RoutingMetricsSnapshot {
        RoutingMetricsSnapshot {
            routes_calculated: self.metrics.routes_calculated.load(Ordering::SeqCst),
            cache_hits: self.metrics.cache_hits.load(Ordering::SeqCst),
            cache_misses: self.metrics.cache_misses.load(Ordering::SeqCst),
            position_lookups: self.metrics.position_lookups.load(Ordering::SeqCst),
            routing_errors: self.metrics.routing_errors.load(Ordering::SeqCst),
            active_client_sessions: self.client_sessions.len() as u64,
            cached_routes: self.routing_cache.len() as u64,
        }
    }
    
    async fn find_server_for_position(&self, position: &Position) -> Result<ServerId> {
        let servers = self.discovery.get_all_servers().await;
        
        for server in servers {
            if matches!(server.status, ServerStatus::Ready) && server.gameworld_bounds.contains(position) {
                return Ok(server.id);
            }
        }
        
        // No server found that contains this position
        Err(AtlasError::Routing(format!(
            "No server found for position ({}, {}, {})", 
            position.x, position.y, position.z
        )))
    }
    
    async fn is_server_healthy(&self, server_id: &ServerId) -> Result<bool> {
        if let Some(server) = self.discovery.get_server(server_id).await? {
            Ok(matches!(server.status, ServerStatus::Ready) && 
               server.last_heartbeat.signed_duration_since(Utc::now()).num_seconds().abs() < 30)
        } else {
            Ok(false)
        }
    }
    
    async fn start_cache_cleanup_task(&self) {
        let routing_cache = Arc::clone(&self.routing_cache);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            
            loop {
                interval.tick().await;
                
                let now = Instant::now();
                routing_cache.retain(|_, entry| {
                    now.duration_since(entry.cached_at) < entry.ttl
                });
            }
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingMetricsSnapshot {
    pub routes_calculated: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub position_lookups: u64,
    pub routing_errors: u64,
    pub active_client_sessions: u64,
    pub cached_routes: u64,
}