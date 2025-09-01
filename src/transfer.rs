use std::net::TcpStream;
use std::sync::Arc;
use serde_json::json;
use crate::client::{ClientConnection, ClientState};
use crate::server::ServerManager;
use crate::error::{ProxyError, Result};

/// Client transfer manager for moving clients between servers
pub struct TransferManager {
    server_manager: Arc<ServerManager>,
}

/// Transfer notification message
#[derive(Debug, Clone)]
pub struct TransferNotification {
    pub client_id: String,
    pub from_server: ServerInfo,
    pub to_server: ServerInfo,
    pub reason: TransferReason,
    pub estimated_downtime_ms: u64,
    pub transfer_token: String,
}

/// Server information for transfer notifications
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub id: String,
    pub addr: String,
    pub load: f32,
    pub health_status: String,
}

/// Reasons for client transfer
#[derive(Debug, Clone)]
pub enum TransferReason {
    /// Load balancing - moving from overloaded server
    LoadBalancing,
    /// Server maintenance
    Maintenance,
    /// Server failure
    ServerFailure,
    /// Manual administrative transfer
    Administrative,
    /// Client requested transfer
    ClientRequested,
}

impl TransferManager {
    /// Create a new transfer manager
    pub fn new(server_manager: Arc<ServerManager>) -> Self {
        Self { server_manager }
    }
    
    /// Initiate client transfer to a better server
    pub fn initiate_transfer(
        &self,
        client: &ClientConnection,
        reason: TransferReason,
    ) -> Result<TransferNotification> {
        // Check if client can be transferred
        if !client.can_transfer() {
            return Err(ProxyError::Transfer(
                "Client is not in a transferable state".to_string()
            ));
        }
        
        // Find the best target server
        let current_server_id = client.current_server();
        let target_server = self.find_best_target_server(&current_server_id)?;
        
        // Create transfer notification
        let notification = self.create_transfer_notification(
            client,
            &current_server_id,
            &target_server.id,
            reason,
        )?;
        
        // Send advance notification to all parties
        self.send_transfer_notifications(&notification)?;
        
        // Update client state
        client.set_state(ClientState::Transferring {
            from_server: current_server_id,
            to_server: target_server.id,
        });
        
        Ok(notification)
    }
    
    /// Execute the actual transfer
    pub fn execute_transfer(
        &self,
        client: &ClientConnection,
        notification: &TransferNotification,
    ) -> Result<TcpStream> {
        // Connect to new server
        let new_server_stream = self.server_manager
            .connect_to_server(&notification.to_server.id)?;
            
        // Update connection counts
        self.server_manager.decrement_connections(&notification.from_server.id)?;
        self.server_manager.increment_connections(&notification.to_server.id)?;
        
        // Send transfer completion notification
        self.send_transfer_completion_notification(notification)?;
        
        // Update client state
        client.set_state(ClientState::Connected);
        
        Ok(new_server_stream)
    }
    
    /// Find the best target server for transfer
    fn find_best_target_server(&self, current_server_id: &str) -> Result<crate::config::ServerConfig> {
        let best_server = self.server_manager.get_best_server()
            .ok_or_else(|| ProxyError::Transfer("No available servers for transfer".to_string()))?;
            
        // Don't transfer to the same server
        if best_server.id == current_server_id {
            return Err(ProxyError::Transfer("No better server available for transfer".to_string()));
        }
        
        Ok(best_server)
    }
    
    /// Create transfer notification message
    fn create_transfer_notification(
        &self,
        client: &ClientConnection,
        from_server_id: &str,
        to_server_id: &str,
        reason: TransferReason,
    ) -> Result<TransferNotification> {
        let from_server = self.server_manager.get_server(from_server_id)
            .ok_or_else(|| ProxyError::Transfer(format!("From server {} not found", from_server_id)))?;
            
        let to_server = self.server_manager.get_server(to_server_id)
            .ok_or_else(|| ProxyError::Transfer(format!("To server {} not found", to_server_id)))?;
        
        Ok(TransferNotification {
            client_id: client.client_id(),
            from_server: ServerInfo {
                id: from_server.id.clone(),
                addr: from_server.addr.to_string(),
                load: from_server.load_percentage(),
                health_status: if from_server.healthy { "healthy".to_string() } else { "unhealthy".to_string() },
            },
            to_server: ServerInfo {
                id: to_server.id.clone(),
                addr: to_server.addr.to_string(),
                load: to_server.load_percentage(),
                health_status: if to_server.healthy { "healthy".to_string() } else { "unhealthy".to_string() },
            },
            reason,
            estimated_downtime_ms: 100, // Estimate 100ms downtime
            transfer_token: self.generate_transfer_token(),
        })
    }
    
    /// Send transfer notifications to all relevant parties
    fn send_transfer_notifications(&self, notification: &TransferNotification) -> Result<()> {
        // Create JSON notification message
        let message = json!({
            "type": "transfer_notification",
            "client_id": notification.client_id,
            "from_server": {
                "id": notification.from_server.id,
                "addr": notification.from_server.addr,
                "load": notification.from_server.load,
                "health": notification.from_server.health_status
            },
            "to_server": {
                "id": notification.to_server.id,
                "addr": notification.to_server.addr,
                "load": notification.to_server.load,
                "health": notification.to_server.health_status
            },
            "reason": format!("{:?}", notification.reason),
            "estimated_downtime_ms": notification.estimated_downtime_ms,
            "transfer_token": notification.transfer_token,
            "timestamp": chrono::Utc::now().to_rfc3339()
        });
        
        println!("[TRANSFER] Notifying transfer: {}", message);
        
        // In production, you would send this to:
        // 1. The current server (to prepare for client disconnect)
        // 2. The target server (to prepare for new client)
        // 3. Any monitoring/logging systems
        // 4. The client (if using a protocol that supports it)
        
        Ok(())
    }
    
    /// Send transfer completion notification
    fn send_transfer_completion_notification(&self, notification: &TransferNotification) -> Result<()> {
        let message = json!({
            "type": "transfer_completed",
            "client_id": notification.client_id,
            "from_server": notification.from_server.id,
            "to_server": notification.to_server.id,
            "transfer_token": notification.transfer_token,
            "timestamp": chrono::Utc::now().to_rfc3339()
        });
        
        println!("[TRANSFER] Transfer completed: {}", message);
        Ok(())
    }
    
    /// Generate a unique transfer token
    fn generate_transfer_token(&self) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("transfer-{}", timestamp)
    }
    
    /// Check if a client should be transferred based on server load
    pub fn should_transfer_for_load_balancing(&self, client: &ClientConnection) -> bool {
        let current_server_id = client.current_server();
        let current_load = self.server_manager.get_server_load(&current_server_id);
        
        // Transfer if current server is over 80% capacity
        if current_load > 0.8 {
            // Check if there's a significantly better server available
            if let Some(best_server) = self.server_manager.get_best_server() {
                let best_load = best_server.load_percentage();
                // Transfer if the best server has at least 20% less load
                return current_load - best_load > 0.2;
            }
        }
        
        false
    }
    
    /// Automatically transfer clients for load balancing
    pub fn auto_transfer_for_load_balancing(&self, clients: &[ClientConnection]) -> Result<Vec<TransferNotification>> {
        let mut notifications = Vec::new();
        
        for client in clients {
            if self.should_transfer_for_load_balancing(client) {
                match self.initiate_transfer(client, TransferReason::LoadBalancing) {
                    Ok(notification) => notifications.push(notification),
                    Err(e) => println!("[TRANSFER] Failed to initiate transfer for {}: {}", client.client_id(), e),
                }
            }
        }
        
        Ok(notifications)
    }
}