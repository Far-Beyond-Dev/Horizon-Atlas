mod config;
mod server;
mod proxy;
mod cluster;
mod encryption;
mod compression;
mod game_server;
mod regions;
mod state;

use anyhow::Result;
use config::Config;
use server::AtlasServer;
use tracing::{info, error};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_logging()?;
    
    let config = Config::load("config.toml").await?;
    info!("Loaded configuration for node: {}", config.cluster.node_id);
    
    let server = AtlasServer::new(config).await?;
    
    info!("Starting Horizon Atlas server...");
    if let Err(e) = server.start().await {
        error!("Server error: {}", e);
        return Err(e);
    }
    
    Ok(())
}

fn init_logging() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();
    Ok(())
}
