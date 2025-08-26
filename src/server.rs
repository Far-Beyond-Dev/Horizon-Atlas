use crate::config::Config;
use crate::errors::{AtlasError, Result};
use crate::types::{ClientId, ServerId};
use axum::{
    extract::State,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::signal;
use tower_http::cors::CorsLayer;
use tracing::{info, error};

pub struct AtlasServer {
    config: Config,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    status: String,
    version: String,
    node_id: String,
}

impl AtlasServer {
    pub async fn new(config: Config) -> Result<Self> {
        info!("Initializing Atlas server with configuration");
        Ok(Self { config })
    }
    
    pub async fn run(self) -> Result<()> {
        info!("Starting Atlas server on {}", self.config.server.bind_address);
        
        let app = Router::new()
            .route("/health", get(health_check))
            .layer(CorsLayer::permissive())
            .with_state(Arc::new(self.config.clone()));

        let listener = tokio::net::TcpListener::bind(self.config.server.bind_address).await?;
        
        info!("Atlas server is running on {}", self.config.server.bind_address);
        
        tokio::select! {
            result = axum::serve(listener, app) => {
                if let Err(e) = result {
                    error!("Server error: {}", e);
                    return Err(AtlasError::Internal(format!("Server error: {}", e)));
                }
            }
            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C signal, shutting down");
            }
        }
        
        info!("Atlas server stopped");
        Ok(())
    }
}

async fn health_check(State(config): State<Arc<Config>>) -> impl IntoResponse {
    let response = HealthResponse {
        status: "healthy".to_string(),
        version: "1.0.0".to_string(),
        node_id: config.cluster.node_id.clone(),
    };
    
    Json(response)
}