use thiserror::Error;

#[derive(Error, Debug)]
pub enum AtlasError {
    #[error("Configuration error: {0}")]
    Config(String),
    
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),
    
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
    
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    
    #[error("Crypto error: {0}")]
    Crypto(String),
    
    #[error("Server not found: {server_id}")]
    ServerNotFound { server_id: String },
    
    #[error("Region not found: {region_id}")]
    RegionNotFound { region_id: String },
    
    #[error("Client connection error: {0}")]
    ClientConnection(String),
    
    #[error("Server connection error: {0}")]
    ServerConnection(String),
    
    #[error("Routing error: {0}")]
    Routing(String),
    
    #[error("Cluster error: {0}")]
    Cluster(String),
    
    #[error("Discovery error: {0}")]
    Discovery(String),
    
    #[error("Health check error: {0}")]
    HealthCheck(String),
    
    #[error("Transition error: {0}")]
    Transition(String),
    
    #[error("Authentication error: {0}")]
    Authentication(String),
    
    #[error("Authorization error: {0}")]
    Authorization(String),
    
    #[error("Rate limit exceeded")]
    RateLimit,
    
    #[error("Service unavailable")]
    ServiceUnavailable,
    
    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<reqwest::Error> for AtlasError {
    fn from(err: reqwest::Error) -> Self {
        AtlasError::Network(std::io::Error::new(std::io::ErrorKind::Other, err))
    }
}

impl From<prometheus::Error> for AtlasError {
    fn from(err: prometheus::Error) -> Self {
        AtlasError::Internal(format!("Prometheus error: {}", err))
    }
}

// Removed conflicting From implementation - anyhow already provides this

pub type Result<T> = std::result::Result<T, AtlasError>;