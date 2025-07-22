use crate::config::Config;
use crate::proxy::WebSocketProxy;
use crate::game_server::GameServerManager;
use crate::server_manager::ServerManager;
use crate::cluster::ClusterManager;
use crate::encryption::EncryptionManager;
use crate::compression::DeltaCompressor;
use anyhow::Result;
use axum::{
    extract::{ws::WebSocketUpgrade, State, ConnectInfo},
    response::Response,
    routing::get,
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::signal;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tracing::{info, error};
use uuid::Uuid;

pub struct AtlasServer {
    config: Config,
    proxy: Arc<WebSocketProxy>,
    game_server_manager: Arc<GameServerManager>,
    server_manager: Arc<ServerManager>,
    cluster_manager: Arc<ClusterManager>,
}

impl AtlasServer {
    pub async fn new(config: Config) -> Result<Self> {
        info!("Initializing Horizon Atlas server...");
        
        let encryption_manager = Arc::new(EncryptionManager::new(
            config.security.enable_encryption,
            "horizon_atlas_secret_key",
        )?);
        
        let compressor = Arc::new(DeltaCompressor::new(
            config.compression.enable,
            config.compression.threshold,
        ));
        
        let game_server_manager = Arc::new(GameServerManager::new(config.game_servers.clone()));
        
        let server_manager = Arc::new(ServerManager::new(
            config.spatial.preload_distance,
            config.spatial.transfer_distance,
        ));
        
        let cluster_manager = Arc::new(ClusterManager::new(
            config.cluster.clone(),
            config.server.address.clone(),
            config.server.port,
        ));
        
        let proxy = Arc::new(WebSocketProxy::new(
            Arc::clone(&game_server_manager),
            Arc::clone(&server_manager),
            Arc::clone(&encryption_manager),
            Arc::clone(&compressor),
        ));
        
        game_server_manager.start_health_monitor().await;
        
        Ok(Self {
            config,
            proxy,
            game_server_manager,
            server_manager,
            cluster_manager,
        })
    }
    
    pub async fn start(self) -> Result<()> {
        let app = self.create_router();
        let addr = format!("{}:{}", self.config.server.address, self.config.server.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        
        info!("Horizon Atlas listening on {}", addr);
        
        
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        
        tokio::select! {
            result = server => {
                if let Err(e) = result {
                    error!("Server error: {}", e);
                }
            }
            _ = signal::ctrl_c() => {
                info!("Received shutdown signal, gracefully shutting down...");
            }
        }
        
        Ok(())
    }
    
    fn create_router(&self) -> Router {
        let app_state = AppState {
            proxy: Arc::clone(&self.proxy),
            game_server_manager: Arc::clone(&self.game_server_manager),
            server_manager: Arc::clone(&self.server_manager),
        };
        
        Router::new()
            .route("/", get(websocket_handler))
            .route("/health", get(health_check))
            .route("/metrics", get(metrics_handler))
            .route("/servers", get(servers_handler))
            .route("/server-bounds", get(server_bounds_handler))
            .with_state(app_state)
            .layer(
                ServiceBuilder::new()
                    .layer(CorsLayer::permissive())
            )
    }
}

#[derive(Clone)]
struct AppState {
    proxy: Arc<WebSocketProxy>,
    game_server_manager: Arc<GameServerManager>,
    server_manager: Arc<ServerManager>,
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Response {
    let client_id = Uuid::new_v4();
    info!("WebSocket connection from {} assigned ID: {}", addr, client_id);
    
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = state.proxy.handle_client_connection(socket, client_id).await {
            error!("WebSocket connection error: {}", e);
        }
    })
}

async fn health_check() -> &'static str {
    "OK"
}

async fn metrics_handler(State(state): State<AppState>) -> Result<String, String> {
    let connection_count = state.proxy.get_connection_count().await;
    let servers = state.game_server_manager.get_all_servers().await;
    let active_servers = servers.iter().filter(|s| matches!(s.status, crate::state::ServerStatus::Running)).count();
    let total_players: usize = servers.iter().map(|s| s.player_count).sum();
    
    let metrics = format!(
        "# HELP atlas_connections_total Total number of active connections\n\
         # TYPE atlas_connections_total gauge\n\
         atlas_connections_total {}\n\
         # HELP atlas_servers_active Number of active game servers\n\
         # TYPE atlas_servers_active gauge\n\
         atlas_servers_active {}\n\
         # HELP atlas_players_total Total number of players across all servers\n\
         # TYPE atlas_players_total gauge\n\
         atlas_players_total {}\n",
        connection_count, active_servers, total_players
    );
    
    Ok(metrics)
}

async fn servers_handler(State(state): State<AppState>) -> Result<String, String> {
    let servers = state.game_server_manager.get_all_servers().await;
    serde_json::to_string_pretty(&servers)
        .map_err(|e| format!("Serialization error: {}", e))
}

async fn server_bounds_handler(State(state): State<AppState>) -> Result<String, String> {
    let servers = state.server_manager.get_all_servers().await;
    serde_json::to_string_pretty(&servers)
        .map_err(|e| format!("Serialization error: {}", e))
}