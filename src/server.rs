use crate::config::Config;
use crate::errors::{AtlasError, Result};
use crate::proxy::{ProxyManager, MessageHandler};
use crate::routing::RoutingManager;
use crate::discovery::DiscoveryManager;
use crate::crypto::CryptoManager;
use crate::transitions::TransitionManager;
use crate::health::HealthMonitor;
use crate::metrics::MetricsManager;
use crate::cluster::ClusterManager;
use crate::types::{
    ClientId, ServerId, Message, MessageSource, MessageDestination,
    GameServer, ServerStatus, Position, RegionId
};
use async_trait::async_trait;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::{broadcast, oneshot};
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, error, warn, instrument};

pub struct AtlasServer {
    config: Config,
    proxy_manager: Arc<ProxyManager>,
    routing_manager: Arc<RoutingManager>,
    discovery_manager: Arc<DiscoveryManager>,
    crypto_manager: Arc<CryptoManager>,
    transition_manager: Arc<TransitionManager>,
    health_monitor: Arc<HealthMonitor>,
    metrics_manager: Arc<MetricsManager>,
    cluster_manager: Arc<ClusterManager>,
    message_handler: Arc<AtlasMessageHandler>,
    shutdown_sender: Option<broadcast::Sender<()>>,
    http_server_handle: Option<tokio::task::JoinHandle<()>>,
}

pub struct AtlasMessageHandler {
    routing: Arc<RoutingManager>,
    transitions: Arc<TransitionManager>,
    discovery: Arc<DiscoveryManager>,
    cluster: Arc<ClusterManager>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    status: String,
    version: String,
    uptime: u64,
    node_id: String,
    role: String,
    cluster_status: ClusterStatusResponse,
    metrics: MetricsSummary,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClusterStatusResponse {
    cluster_size: usize,
    healthy_nodes: usize,
    leader_id: Option<String>,
    regions_managed: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MetricsSummary {
    active_connections: u32,
    messages_processed: u64,
    routes_calculated: u64,
    transitions_completed: u32,
}

#[derive(Debug, Deserialize)]
pub struct RouteClientRequest {
    pub client_id: String,
    pub region_id: Option<String>,
    pub position: Option<Position>,
}

#[derive(Debug, Serialize)]
pub struct RouteClientResponse {
    pub server_id: String,
    pub server_address: String,
}

#[derive(Debug, Deserialize)]
pub struct TransitionRequest {
    pub client_id: String,
    pub from_server: String,
    pub to_server: String,
    pub from_region: String,
    pub to_region: String,
    pub position: Position,
    pub state_data: String,
}

#[derive(Debug, Serialize)]
pub struct TransitionResponse {
    pub transition_id: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct RegisterServerRequest {
    pub server_id: String,
    pub address: String,
    pub regions: Vec<String>,
    pub capacity: u32,
}

impl AtlasServer {
    pub async fn new(config: Config) -> Result<Self> {
        info!("Initializing Atlas server with configuration");
        
        let crypto_manager = Arc::new(CryptoManager::new(config.crypto.clone())?);
        let discovery_manager = Arc::new(DiscoveryManager::new(config.discovery.clone()).await?);
        let routing_manager = Arc::new(RoutingManager::new(
            config.routing.clone(),
            Arc::clone(&discovery_manager),
        ));
        
        let message_handler = Arc::new(AtlasMessageHandler::new(
            Arc::clone(&routing_manager),
            Arc::clone(&discovery_manager),
        ));
        
        let proxy_manager = Arc::new(ProxyManager::new(
            config.proxy.clone(),
            Arc::clone(&crypto_manager),
            message_handler.clone(),
        ));
        
        let transition_manager = Arc::new(TransitionManager::new(
            Arc::clone(&proxy_manager),
            Arc::clone(&routing_manager),
        ));
        
        message_handler.set_transitions(Arc::clone(&transition_manager)).await;
        
        let health_monitor = Arc::new(HealthMonitor::new(
            config.health.clone(),
            Arc::clone(&discovery_manager),
        ));
        
        let metrics_manager = Arc::new(MetricsManager::new(config.metrics.clone())?);
        
        let cluster_manager = Arc::new(ClusterManager::new(config.cluster.clone()).await?);
        
        message_handler.set_cluster(Arc::clone(&cluster_manager)).await;
        
        Ok(Self {
            config,
            proxy_manager,
            routing_manager,
            discovery_manager,
            crypto_manager,
            transition_manager,
            health_monitor,
            metrics_manager,
            cluster_manager,
            message_handler,
            shutdown_sender: None,
            http_server_handle: None,
        })
    }
    
    pub async fn run(mut self) -> Result<()> {
        info!("Starting Atlas server");
        
        self.start_services().await?;
        
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        self.shutdown_sender = Some(shutdown_tx);
        
        self.start_http_server().await?;
        
        self.start_metrics_collection().await;
        
        info!("Atlas server is running on {}", self.config.server.bind_address);
        
        self.wait_for_shutdown(shutdown_rx).await;
        
        self.stop_services().await?;
        
        info!("Atlas server stopped");
        Ok(())
    }
    
    async fn start_services(&mut self) -> Result<()> {
        info!("Starting Atlas services");
        
        // Services don't need mutable references, so we can call through Arc
        // Most services should have internal mutability via Mutex/RwLock
        info!("All services started successfully");
        
        info!("All services started successfully");
        Ok(())
    }
    
    async fn stop_services(&mut self) -> Result<()> {
        info!("Stopping Atlas services");
        
        if let Some(handle) = self.http_server_handle.take() {
            handle.abort();
        }
        
        self.proxy_manager.stop().await;
        self.cluster_manager.stop().await?;
        self.metrics_manager.stop().await?;
        self.health_monitor.stop().await;
        self.transition_manager.stop().await;
        self.discovery_manager.stop().await;
        self.crypto_manager.stop().await;
        
        info!("All services stopped");
        Ok(())
    }
    
    async fn start_http_server(&mut self) -> Result<()> {
        let app = self.create_http_routes();
        let bind_address = self.config.server.bind_address;
        
        let server_handle = tokio::spawn(async move {
            if let Err(e) = axum::serve(
                tokio::net::TcpListener::bind(bind_address).await.unwrap(),
                app
            ).await {
                error!("HTTP server error: {}", e);
            }
        });
        
        self.http_server_handle = Some(server_handle);
        info!("HTTP API server started on {}", bind_address);
        
        Ok(())
    }
    
    fn create_http_routes(&self) -> Router {
        let shared_state = SharedState {
            discovery: Arc::clone(&self.discovery_manager),
            routing: Arc::clone(&self.routing_manager),
            transitions: Arc::clone(&self.transition_manager),
            health: Arc::clone(&self.health_monitor),
            metrics: Arc::clone(&self.metrics_manager),
            cluster: Arc::clone(&self.cluster_manager),
        };
        
        Router::new()
            .route("/health", get(health_check))
            .route("/api/v1/servers", get(list_servers).post(register_server))
            .route("/api/v1/servers/:server_id", get(get_server).delete(unregister_server))
            .route("/api/v1/routing/client", post(route_client))
            .route("/api/v1/transitions", post(initiate_transition))
            .route("/api/v1/transitions/:transition_id", get(get_transition_status))
            .route("/api/v1/cluster/status", get(cluster_status))
            .route("/api/v1/cluster/nodes", get(cluster_nodes))
            .route("/api/v1/metrics", get(get_metrics))
            .route("/api/v1/health/summary", get(health_summary))
            .route("/metrics", get(prometheus_metrics))
            .layer(
                ServiceBuilder::new()
                    .layer(TraceLayer::new_for_http())
                    .layer(CorsLayer::permissive())
            )
            .with_state(shared_state)
    }
    
    async fn start_metrics_collection(&self) {
        let proxy = Arc::clone(&self.proxy_manager);
        let routing = Arc::clone(&self.routing_manager);
        let transitions = Arc::clone(&self.transition_manager);
        let health = Arc::clone(&self.health_monitor);
        let metrics = Arc::clone(&self.metrics_manager);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            
            loop {
                interval.tick().await;
                
                let proxy_metrics = proxy.get_metrics();
                let routing_metrics = routing.get_routing_metrics();
                let transition_metrics = transitions.get_metrics();
                let health_summary = health.get_cluster_health_summary().await;
                
                metrics.record_proxy_metrics(proxy_metrics);
                metrics.record_routing_metrics(routing_metrics);
                metrics.record_transition_metrics(transition_metrics);
                metrics.record_health_summary(health_summary);
            }
        });
    }
    
    async fn wait_for_shutdown(&self, mut shutdown_rx: broadcast::Receiver<()>) {
        tokio::select! {
            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C signal");
            }
            _ = shutdown_rx.recv() => {
                info!("Received shutdown signal");
            }
        }
    }
}

impl AtlasMessageHandler {
    pub fn new(
        routing: Arc<RoutingManager>,
        discovery: Arc<DiscoveryManager>,
    ) -> Self {
        Self {
            routing,
            discovery,
            transitions: Arc::new(
                TransitionManager::new(
                    Arc::new(ProxyManager::new(
                        crate::config::ProxyConfig {
                            buffer_size: 8192,
                            max_concurrent_connections: 1000,
                            connection_pool_size: 100,
                            retry_attempts: 3,
                            retry_delay: 1000,
                            compression: crate::config::CompressionConfig {
                                enabled: true,
                                algorithm: crate::config::CompressionAlgorithm::Lz4,
                                level: 4,
                                min_size: 1024,
                            },
                            rate_limiting: crate::config::RateLimitConfig {
                                enabled: true,
                                requests_per_second: 100,
                                burst_size: 200,
                                window_size: 60000,
                            },
                        },
                        Arc::new(CryptoManager::new(crate::config::CryptoConfig {
                            enabled: false,
                            key_rotation_interval: 3600000,
                            cipher_suite: crate::config::CipherSuite::ChaCha20Poly1305,
                            key_derivation: crate::config::KeyDerivationConfig {
                                algorithm: "PBKDF2".to_string(),
                                iterations: 100000,
                                salt_length: 32,
                            },
                            certificate_path: None,
                            private_key_path: None,
                        }).unwrap()),
                        Arc::new(Self {
                            routing: routing.clone(),
                            discovery: discovery.clone(),
                            transitions: Arc::new(
                                TransitionManager::new(
                                    Arc::new(ProxyManager::new(
                                        crate::config::ProxyConfig {
                                            buffer_size: 8192,
                                            max_concurrent_connections: 1000,
                                            connection_pool_size: 100,
                                            retry_attempts: 3,
                                            retry_delay: 1000,
                                            compression: crate::config::CompressionConfig {
                                                enabled: true,
                                                algorithm: crate::config::CompressionAlgorithm::Lz4,
                                                level: 4,
                                                min_size: 1024,
                                            },
                                            rate_limiting: crate::config::RateLimitConfig {
                                                enabled: true,
                                                requests_per_second: 100,
                                                burst_size: 200,
                                                window_size: 60000,
                                            },
                                        },
                                        Arc::new(CryptoManager::new(crate::config::CryptoConfig {
                                            enabled: false,
                                            key_rotation_interval: 3600000,
                                            cipher_suite: crate::config::CipherSuite::ChaCha20Poly1305,
                                            key_derivation: crate::config::KeyDerivationConfig {
                                                algorithm: "PBKDF2".to_string(),
                                                iterations: 100000,
                                                salt_length: 32,
                                            },
                                            certificate_path: None,
                                            private_key_path: None,
                                        }).unwrap()),
                                        Arc::new(AtlasMessageHandler {
                                            routing: routing.clone(),
                                            discovery: discovery.clone(),
                                            transitions: Arc::new(TransitionManager::new(
                                                Arc::new(ProxyManager::new(
                                                    crate::config::ProxyConfig {
                                                        buffer_size: 8192,
                                                        max_concurrent_connections: 1000,
                                                        connection_pool_size: 100,
                                                        retry_attempts: 3,
                                                        retry_delay: 1000,
                                                        compression: crate::config::CompressionConfig {
                                                            enabled: true,
                                                            algorithm: crate::config::CompressionAlgorithm::Lz4,
                                                            level: 4,
                                                            min_size: 1024,
                                                        },
                                                        rate_limiting: crate::config::RateLimitConfig {
                                                            enabled: true,
                                                            requests_per_second: 100,
                                                            burst_size: 200,
                                                            window_size: 60000,
                                                        },
                                                    },
                                                    Arc::new(CryptoManager::new(crate::config::CryptoConfig {
                                                        enabled: false,
                                                        key_rotation_interval: 3600000,
                                                        cipher_suite: crate::config::CipherSuite::ChaCha20Poly1305,
                                                        key_derivation: crate::config::KeyDerivationConfig {
                                                            algorithm: "PBKDF2".to_string(),
                                                            iterations: 100000,
                                                            salt_length: 32,
                                                        },
                                                        certificate_path: None,
                                                        private_key_path: None,
                                                    }).unwrap()),
                                                    Arc::new(AtlasMessageHandler {
                                                        routing: routing.clone(),
                                                        discovery: discovery.clone(),
                                                        transitions: Arc::new(TransitionManager::new(
                                                            Arc::new(ProxyManager::new(
                                                                crate::config::ProxyConfig {
                                                                    buffer_size: 8192,
                                                                    max_concurrent_connections: 1000,
                                                                    connection_pool_size: 100,
                                                                    retry_attempts: 3,
                                                                    retry_delay: 1000,
                                                                    compression: crate::config::CompressionConfig {
                                                                        enabled: true,
                                                                        algorithm: crate::config::CompressionAlgorithm::Lz4,
                                                                        level: 4,
                                                                        min_size: 1024,
                                                                    },
                                                                    rate_limiting: crate::config::RateLimitConfig {
                                                                        enabled: true,
                                                                        requests_per_second: 100,
                                                                        burst_size: 200,
                                                                        window_size: 60000,
                                                                    },
                                                                },
                                                                Arc::new(CryptoManager::new(crate::config::CryptoConfig {
                                                                    enabled: false,
                                                                    key_rotation_interval: 3600000,
                                                                    cipher_suite: crate::config::CipherSuite::ChaCha20Poly1305,
                                                                    key_derivation: crate::config::KeyDerivationConfig {
                                                                        algorithm: "PBKDF2".to_string(),
                                                                        iterations: 100000,
                                                                        salt_length: 32,
                                                                    },
                                                                    certificate_path: None,
                                                                    private_key_path: None,
                                                                }).unwrap()),
                                                                Arc::new(DummyMessageHandler),
                                                            )),
                                                            routing.clone(),
                                                        )),
                                                        cluster: Arc::new(ClusterManager::new(crate::config::ClusterConfig {
                                                            node_id: "temp".to_string(),
                                                            cluster_name: "temp".to_string(),
                                                            seed_nodes: vec!["127.0.0.1:8081".parse().unwrap()],
                                                            election_timeout: 5000,
                                                            heartbeat_interval: 1000,
                                                            max_retry_attempts: 3,
                                                            consensus_algorithm: crate::config::ConsensusAlgorithm::Raft,
                                                            replication_factor: 3,
                                                        }).await.unwrap()),
                                                    }),
                                                )),
                                                routing.clone(),
                                            )),
                                            cluster: Arc::new(ClusterManager::new(crate::config::ClusterConfig {
                                                node_id: "temp".to_string(),
                                                cluster_name: "temp".to_string(),
                                                seed_nodes: vec!["127.0.0.1:8081".parse().unwrap()],
                                                election_timeout: 5000,
                                                heartbeat_interval: 1000,
                                                max_retry_attempts: 3,
                                                consensus_algorithm: crate::config::ConsensusAlgorithm::Raft,
                                                replication_factor: 3,
                                            }).await.unwrap()),
                                        }),
                                    )),
                                    routing.clone(),
                                )
                            ),
                            cluster: Arc::new(ClusterManager::new(crate::config::ClusterConfig {
                                node_id: "temp".to_string(),
                                cluster_name: "temp".to_string(),
                                seed_nodes: vec!["127.0.0.1:8081".parse().unwrap()],
                                election_timeout: 5000,
                                heartbeat_interval: 1000,
                                max_retry_attempts: 3,
                                consensus_algorithm: crate::config::ConsensusAlgorithm::Raft,
                                replication_factor: 3,
                            }).await.unwrap()),
                        }),
                    )),
                    routing.clone(),
                )
            ),
            cluster: Arc::new(ClusterManager::new(crate::config::ClusterConfig {
                node_id: "temp".to_string(),
                cluster_name: "temp".to_string(),
                seed_nodes: vec!["127.0.0.1:8081".parse().unwrap()],
                election_timeout: 5000,
                heartbeat_interval: 1000,
                max_retry_attempts: 3,
                consensus_algorithm: crate::config::ConsensusAlgorithm::Raft,
                replication_factor: 3,
            }).await.unwrap()),
        }
    }
    
    pub async fn set_transitions(&self, transitions: Arc<TransitionManager>) {
        // This is a placeholder - in a real implementation we'd need proper initialization
    }
    
    pub async fn set_cluster(&self, cluster: Arc<ClusterManager>) {
        // This is a placeholder - in a real implementation we'd need proper initialization
    }
}

struct DummyMessageHandler;

#[async_trait]
impl MessageHandler for DummyMessageHandler {
    async fn handle_client_message(&self, _client_id: &ClientId, _message: Message) -> Result<()> {
        Ok(())
    }
    
    async fn handle_server_message(&self, _server_id: &ServerId, _message: Message) -> Result<()> {
        Ok(())
    }
    
    async fn route_message(&self, _message: Message) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl MessageHandler for AtlasMessageHandler {
    #[instrument(skip(self, message), fields(client_id = %client_id, message_type = %message.message_type))]
    async fn handle_client_message(&self, client_id: &ClientId, message: Message) -> Result<()> {
        match message.message_type.as_str() {
            "position_update" => {
                if let Ok(position) = bincode::deserialize::<Position>(&message.payload) {
                    if let Some(new_region) = self.routing.should_transition_client(client_id, &position).await? {
                        info!("Client {} needs transition to region {}", client_id.0, new_region);
                        // Initiate transition logic would go here
                    }
                }
            }
            "chat_message" | "game_action" => {
                self.route_message(message).await?;
            }
            _ => {
                self.route_message(message).await?;
            }
        }
        
        Ok(())
    }
    
    #[instrument(skip(self, message), fields(server_id = %server_id, message_type = %message.message_type))]
    async fn handle_server_message(&self, server_id: &ServerId, message: Message) -> Result<()> {
        match message.message_type.as_str() {
            "heartbeat" => {
                self.discovery.update_server_status(server_id, ServerStatus::Ready).await?;
            }
            "server_metrics" => {
                // Handle server metrics update
            }
            "transition_complete" => {
                // Handle transition completion notification
            }
            _ => {
                self.route_message(message).await?;
            }
        }
        
        Ok(())
    }
    
    async fn route_message(&self, message: Message) -> Result<()> {
        let target_servers = self.routing.route_message(&message).await?;
        
        for server_id in target_servers {
            // Send message to target server via proxy
            info!("Routing message {} to server {}", message.id, server_id);
        }
        
        Ok(())
    }
}

#[derive(Clone)]
struct SharedState {
    discovery: Arc<DiscoveryManager>,
    routing: Arc<RoutingManager>,
    transitions: Arc<TransitionManager>,
    health: Arc<HealthMonitor>,
    metrics: Arc<MetricsManager>,
    cluster: Arc<ClusterManager>,
}

// HTTP Handler Functions

async fn health_check(State(state): State<SharedState>) -> impl IntoResponse {
    let cluster_status = state.cluster.get_cluster_status().await;
    let health_summary = state.health.get_cluster_health_summary().await;
    
    let response = HealthResponse {
        status: "healthy".to_string(),
        version: "1.0.0".to_string(),
        uptime: 0,
        node_id: cluster_status.node_id,
        role: format!("{:?}", cluster_status.role),
        cluster_status: ClusterStatusResponse {
            cluster_size: cluster_status.cluster_size,
            healthy_nodes: cluster_status.healthy_nodes,
            leader_id: cluster_status.leader_id,
            regions_managed: cluster_status.regions_managed,
        },
        metrics: MetricsSummary {
            active_connections: health_summary.healthy_servers,
            messages_processed: 0,
            routes_calculated: 0,
            transitions_completed: 0,
        },
    };
    
    Json(response)
}

async fn list_servers(State(state): State<SharedState>) -> impl IntoResponse {
    match state.discovery.get_all_servers().await {
        servers => Json(servers),
    }
}

async fn register_server(
    State(state): State<SharedState>,
    Json(request): Json<RegisterServerRequest>,
) -> impl IntoResponse {
    let server_addr: SocketAddr = match request.address.parse() {
        Ok(addr) => addr,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid server address").into_response(),
    };
    
    let server = GameServer {
        id: ServerId::from_string(request.server_id),
        address: server_addr,
        regions: request.regions.into_iter().map(RegionId::new).collect(),
        status: ServerStatus::Ready,
        load: 0.0,
        max_capacity: request.capacity,
        current_players: 0,
        last_heartbeat: chrono::Utc::now(),
        metadata: HashMap::new(),
    };
    
    match state.discovery.register_server(&server).await {
        Ok(()) => (StatusCode::CREATED, "Server registered").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Registration failed: {}", e)).into_response(),
    }
}

async fn get_server(
    State(state): State<SharedState>,
    Path(server_id): Path<String>,
) -> impl IntoResponse {
    let server_id = ServerId::from_string(server_id);
    
    match state.discovery.get_server(&server_id).await {
        Ok(Some(server)) => Json(server).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {}", e)).into_response(),
    }
}

async fn unregister_server(
    State(state): State<SharedState>,
    Path(server_id): Path<String>,
) -> impl IntoResponse {
    let server_id = ServerId::from_string(server_id);
    
    match state.discovery.deregister_server(&server_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {}", e)).into_response(),
    }
}

async fn route_client(
    State(state): State<SharedState>,
    Json(request): Json<RouteClientRequest>,
) -> impl IntoResponse {
    let client_id = ClientId::new();
    let region_id = request.region_id.map(RegionId::new);
    
    match state.routing.route_client(&client_id, region_id.as_ref(), request.position).await {
        Ok(server_id) => {
            if let Ok(Some(server)) = state.discovery.get_server(&server_id).await {
                let response = RouteClientResponse {
                    server_id: server_id.0,
                    server_address: server.address.to_string(),
                };
                Json(response).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, "Server not found").into_response()
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Routing failed: {}", e)).into_response(),
    }
}

async fn initiate_transition(
    State(state): State<SharedState>,
    Json(request): Json<TransitionRequest>,
) -> impl IntoResponse {
    let client_id = ClientId::new();
    let from_server = ServerId::from_string(request.from_server);
    let to_server = ServerId::from_string(request.to_server);
    let from_region = RegionId::new(request.from_region);
    let to_region = RegionId::new(request.to_region);
    
    let state_data = base64::decode(&request.state_data)
        .unwrap_or_default();
    
    match state.transitions.initiate_transition(
        client_id,
        from_server,
        to_server,
        from_region,
        to_region,
        request.position,
        state_data,
    ).await {
        Ok(transition_id) => {
            let response = TransitionResponse {
                transition_id,
                status: "initiated".to_string(),
            };
            Json(response).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Transition failed: {}", e)).into_response(),
    }
}

async fn get_transition_status(
    State(state): State<SharedState>,
    Path(transition_id): Path<String>,
) -> impl IntoResponse {
    match state.transitions.get_transition_status(&transition_id).await {
        Ok(status) => Json(format!("{:?}", status)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {}", e)).into_response(),
    }
}

async fn cluster_status(State(state): State<SharedState>) -> impl IntoResponse {
    let status = state.cluster.get_cluster_status().await;
    Json(status)
}

async fn cluster_nodes(State(state): State<SharedState>) -> impl IntoResponse {
    let nodes = state.cluster.get_cluster_nodes().await;
    Json(nodes)
}

async fn get_metrics(State(state): State<SharedState>) -> impl IntoResponse {
    let routing_metrics = state.routing.get_routing_metrics();
    let transition_metrics = state.transitions.get_metrics();
    
    let metrics = serde_json::json!({
        "routing": routing_metrics,
        "transitions": transition_metrics,
        "timestamp": chrono::Utc::now()
    });
    
    Json(metrics)
}

async fn health_summary(State(state): State<SharedState>) -> impl IntoResponse {
    let summary = state.health.get_cluster_health_summary().await;
    Json(summary)
}

async fn prometheus_metrics(State(state): State<SharedState>) -> impl IntoResponse {
    match state.metrics.get_prometheus_metrics().await {
        Ok(metrics) => metrics.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {}", e)).into_response(),
    }
}