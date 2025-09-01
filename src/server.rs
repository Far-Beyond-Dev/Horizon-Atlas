use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use crate::config::ServerConfig;
use crate::error::{ProxyError, Result};

/// Server connection manager
#[derive(Debug)]
pub struct ServerManager {
    /// Available servers
    pub servers: Arc<Mutex<HashMap<String, ServerConfig>>>,
}

impl ServerManager {
    /// Create a new server manager
    pub fn new(servers: Vec<ServerConfig>) -> Self {
        let server_map: HashMap<String, ServerConfig> = servers
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
            
        Self {
            servers: Arc::new(Mutex::new(server_map)),
        }
    }
    
    /// Connect to a specific server
    pub fn connect_to_server(&self, server_id: &str) -> Result<TcpStream> {
        let servers = self.servers.lock().unwrap();
        let server = servers.get(server_id)
            .ok_or_else(|| ProxyError::Connection(format!("Server {} not found", server_id)))?;
            
        if !server.healthy {
            return Err(ProxyError::Connection(format!("Server {} is unhealthy", server_id)));
        }
        
        TcpStream::connect(server.addr)
            .map_err(|e| ProxyError::Connection(format!("Failed to connect to server {}: {}", server_id, e)))
    }
    
    /// Get server by ID
    pub fn get_server(&self, server_id: &str) -> Option<ServerConfig> {
        self.servers.lock().unwrap().get(server_id).cloned()
    }
    
    /// Update server connection count
    pub fn increment_connections(&self, server_id: &str) -> Result<()> {
        let mut servers = self.servers.lock().unwrap();
        if let Some(server) = servers.get_mut(server_id) {
            server.active_connections += 1;
            server.load = server.load_percentage();
            Ok(())
        } else {
            Err(ProxyError::Connection(format!("Server {} not found", server_id)))
        }
    }
    
    /// Decrement server connection count
    pub fn decrement_connections(&self, server_id: &str) -> Result<()> {
        let mut servers = self.servers.lock().unwrap();
        if let Some(server) = servers.get_mut(server_id) {
            if server.active_connections > 0 {
                server.active_connections -= 1;
            }
            server.load = server.load_percentage();
            Ok(())
        } else {
            Err(ProxyError::Connection(format!("Server {} not found", server_id)))
        }
    }
    
    /// Get the best available server for new connections
    pub fn get_best_server(&self) -> Option<ServerConfig> {
        let servers = self.servers.lock().unwrap();
        let available_servers: Vec<&ServerConfig> = servers
            .values()
            .filter(|s| s.can_accept_connections())
            .collect();
            
        if available_servers.is_empty() {
            return None;
        }
        
        // Use least connections algorithm
        available_servers
            .iter()
            .min_by_key(|s| s.active_connections)
            .map(|&s| s.clone())
    }
    
    /// Update server health status
    pub fn update_server_health(&self, server_id: &str, healthy: bool) -> Result<()> {
        let mut servers = self.servers.lock().unwrap();
        if let Some(server) = servers.get_mut(server_id) {
            server.healthy = healthy;
            Ok(())
        } else {
            Err(ProxyError::Connection(format!("Server {} not found", server_id)))
        }
    }
    
    /// Get all server statistics
    pub fn get_server_stats(&self) -> Vec<ServerConfig> {
        self.servers.lock().unwrap().values().cloned().collect()
    }
    
    /// Health check for a server
    pub fn health_check(&self, server_id: &str) -> bool {
        if let Some(server) = self.get_server(server_id) {
            // Simple TCP connection test
            match TcpStream::connect(server.addr) {
                Ok(_) => {
                    let _ = self.update_server_health(server_id, true);
                    true
                }
                Err(_) => {
                    let _ = self.update_server_health(server_id, false);
                    false
                }
            }
        } else {
            false
        }
    }
    
    /// Get server load for a specific server
    pub fn get_server_load(&self, server_id: &str) -> f32 {
        self.servers.lock().unwrap()
            .get(server_id)
            .map(|s| s.load_percentage())
            .unwrap_or(1.0)
    }
    
    /// Check if server can accept new connections
    pub fn can_accept_connections(&self, server_id: &str) -> bool {
        self.servers.lock().unwrap()
            .get(server_id)
            .map(|s| s.can_accept_connections())
            .unwrap_or(false)
    }
}