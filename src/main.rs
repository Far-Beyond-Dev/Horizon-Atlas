mod config;
mod server;
mod proxy;
mod routing;
mod crypto;
mod discovery;
mod cluster;
mod health;
mod metrics;
mod transitions;
mod errors;
mod types;

use anyhow::Result;
use config::Config;
use server::AtlasServer;
use tracing::{info, error};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting Horizon Atlas...");

    let config = Config::load()?;
    info!("Configuration loaded successfully");

    let server = AtlasServer::new(config).await?;
    info!("Atlas server initialized");

    if let Err(e) = server.run().await {
        error!("Atlas server error: {}", e);
        return Err(e);
    }

    Ok(())
}
