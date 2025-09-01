//! # Horizon Atlas - Production-Ready WebSocket Proxy
//!
//! A high-performance, multi-server TCP proxy with advanced features:
//! - Load balancing across multiple backend servers
//! - Automatic client transfers between servers
//! - Real-time data skimming and analysis
//! - Health monitoring and failover
//! - Production-ready error handling and logging
//!
//! ## Features
//!
//! - **Multi-Server Support**: Configure multiple backend servers with automatic load balancing
//! - **Client Transfers**: Seamlessly move clients between servers for load balancing or maintenance
//! - **Data Skimming**: Non-blocking analysis of traffic for monitoring and debugging
//! - **Health Checks**: Automatic monitoring of server health with failover capabilities
//! - **Production Ready**: Comprehensive error handling, logging, and monitoring
//!
//! ## Usage
//!
//! ```bash
//! cargo run
//! ```
//!
//! The proxy will start listening on `0.0.0.0:9000` and forward traffic to configured backend servers.

use horizon_atlas::config::ProxyConfig;
use horizon_atlas::proxy::HorizonProxy;
use horizon_atlas::error::Result;

fn main() -> Result<()> {
    println!("🚀 Starting Horizon Atlas Proxy...");
    
    // Create proxy configuration
    // In production, this would be loaded from a config file or environment variables
    let config = ProxyConfig::new(
        "0.0.0.0:9000",
        vec![
            ("127.0.0.1:8080", "game-server-1"),
            ("127.0.0.1:8081", "game-server-2"),
            ("127.0.0.1:8082", "game-server-3"),
        ],
    )?;
    
    println!("📋 Configuration:");
    println!("   Listen Address: {}", config.listen_addr);
    println!("   Backend Servers: {}", config.servers.len());
    for server in &config.servers {
        println!("     - {} ({})", server.id, server.addr);
    }
    println!("   Buffer Size: {} bytes", config.buffer_size);
    println!("   Max Connections: {}", config.max_connections);
    println!("   Spatial Routing: Region size {}, Prediction time {}s", 
             config.spatial_config.default_region_size, 
             config.spatial_config.prediction_time);
    println!();
    
    // Create and start the proxy
    let proxy = HorizonProxy::new(config)?;
    
    // Start the proxy (this blocks)
    proxy.start()?;
    
    Ok(())
}