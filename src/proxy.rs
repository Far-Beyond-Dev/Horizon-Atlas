use crate::state::{ConnectionInfo, PlayerState};
use crate::game_server::GameServerManager;
use crate::server_manager::{ServerManager, ServerTransition};
use crate::encryption::EncryptionManager;
use crate::compression::DeltaCompressor;
use anyhow::{Result, anyhow};
use axum::extract::ws::{WebSocket, Message};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{info, warn, error, debug};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyMessage {
    pub message_type: MessageType,
    pub player_id: Option<Uuid>,
    pub data: Vec<u8>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageType {
    PlayerConnect,
    PlayerDisconnect,
    PlayerMove,
    PlayerAction,
    ServerTransfer,
    Heartbeat,
    StateSync,
}

pub struct WebSocketProxy {
    connections: Arc<DashMap<Uuid, ConnectionInfo>>,
    server_connections: Arc<DashMap<Uuid, ServerConnection>>,
    game_server_manager: Arc<GameServerManager>,
    server_manager: Arc<ServerManager>,  // Renamed from region_manager
    encryption_manager: Arc<EncryptionManager>,
    compressor: Arc<DeltaCompressor>,
    active_transitions: Arc<DashMap<Uuid, ServerTransition>>,
}

struct ServerConnection {
    sender: mpsc::UnboundedSender<Vec<u8>>,
    server_id: Uuid,  // Server ID is now UUID
}

impl WebSocketProxy {
    pub fn new(
        game_server_manager: Arc<GameServerManager>,
        server_manager: Arc<ServerManager>,
        encryption_manager: Arc<EncryptionManager>,
        compressor: Arc<DeltaCompressor>,
    ) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            server_connections: Arc::new(DashMap::new()),
            game_server_manager,
            server_manager,
            encryption_manager,
            compressor,
            active_transitions: Arc::new(DashMap::new()),
        }
    }
    
    pub async fn handle_client_connection(&self, mut socket: WebSocket, client_id: Uuid) -> Result<()> {
        info!("New client connection: {}", client_id);
        
        let connection_info = ConnectionInfo {
            client_id,
            player_id: None,
            current_server: None,
            target_server: None,
            connection_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        
        self.connections.insert(client_id, connection_info);
        
        while let Some(msg) = socket.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    if let Err(e) = self.handle_client_message(client_id, data).await {
                        warn!("Error handling client message: {}", e);
                    }
                }
                Ok(Message::Text(text)) => {
                    if let Err(e) = self.handle_client_message(client_id, text.into_bytes()).await {
                        warn!("Error handling client text message: {}", e);
                    }
                }
                Ok(Message::Close(_)) => {
                    info!("Client {} disconnected", client_id);
                    break;
                }
                Err(e) => {
                    error!("WebSocket error for client {}: {}", client_id, e);
                    break;
                }
                _ => {}
            }
        }
        
        self.cleanup_client_connection(client_id).await;
        Ok(())
    }
    
    
    async fn handle_client_message(&self, client_id: Uuid, data: Vec<u8>) -> Result<()> {
        let decrypted = self.encryption_manager.decrypt(&data)?;
        
        let proxy_message: ProxyMessage = bincode::deserialize(&decrypted)?;
        debug!("Received message from client {}: {:?}", client_id, proxy_message.message_type);
        
        match proxy_message.message_type {
            MessageType::PlayerConnect => {
                self.handle_player_connect(client_id, proxy_message).await?;
            }
            MessageType::PlayerMove => {
                self.handle_player_move(client_id, proxy_message).await?;
            }
            MessageType::PlayerAction => {
                self.forward_to_game_server(client_id, proxy_message).await?;
            }
            MessageType::Heartbeat => {
                self.handle_heartbeat(client_id).await?;
            }
            _ => {
                warn!("Unhandled message type from client");
            }
        }
        
        Ok(())
    }
    
    async fn handle_player_connect(&self, client_id: Uuid, message: ProxyMessage) -> Result<()> {
        let player_state: PlayerState = bincode::deserialize(&message.data)?;
        let player_id = player_state.id;
        
        info!("Player {} connecting via client {}", player_id, client_id);
        
        // Find appropriate server for player's initial position
        let server_id = self.server_manager
            .find_server_for_position(&player_state.position)
            .await
            .ok_or_else(|| anyhow!("No server available for player position"))?;
        
        info!("Assigning player {} to server {}", player_id, server_id);
        
        if let Some(mut connection) = self.connections.get_mut(&client_id) {
            connection.player_id = Some(player_id);
            connection.current_server = Some(server_id);
        }
        
        // Update server manager with player's position and server assignment
        self.server_manager
            .update_player_position(player_id, player_state.position.clone(), server_id)
            .await?;
        
        self.ensure_server_connection(server_id).await?;
        self.forward_to_game_server(client_id, message).await?;
        
        Ok(())
    }
    
    async fn handle_player_move(&self, client_id: Uuid, message: ProxyMessage) -> Result<()> {
        let player_state: PlayerState = bincode::deserialize(&message.data)?;
        let player_id = player_state.id;
        
        debug!(
            "Processing movement for player {} to position ({:.2}, {:.2}, {:.2})",
            player_id, player_state.position.x, player_state.position.y, player_state.position.z
        );
        
        let connection = self.connections.get(&client_id)
            .ok_or_else(|| anyhow!("Connection not found for client {}", client_id))?;
        
        let current_server = connection.current_server
            .ok_or_else(|| anyhow!("No current server for client {}", client_id))?;
        
        // Update player position in server manager
        self.server_manager
            .update_player_position(player_id, player_state.position.clone(), current_server)
            .await?;
        
        // Check if player needs to be transferred to a different server
        if let Some(target_server) = self.server_manager
            .should_transfer_player(&player_state.position, current_server)
            .await
        {
            info!(
                "Player {} needs transfer from server {} to server {} due to movement",
                player_id, current_server, target_server
            );
            
            self.initiate_server_transfer(client_id, player_id, current_server, target_server).await?;
        } else {
            // No transfer needed, forward movement to current server
            self.forward_to_game_server(client_id, message).await?;
        }
        
        Ok(())
    }
    
    async fn initiate_server_transfer(&self, client_id: Uuid, player_id: Uuid, from_server: Uuid, to_server: Uuid) -> Result<()> {
        if from_server == to_server {
            debug!("No transfer needed - same server {}", from_server);
            return Ok(());
        }
        
        info!(
            "Initiating server transfer for player {} from server {} to server {}", 
            player_id, from_server, to_server
        );
        
        // Create transition record
        let transition = ServerTransition::new(
            player_id,
            from_server,
            to_server,
        );
        
        self.active_transitions.insert(player_id, transition);
        
        // Ensure connection to target server exists
        self.ensure_server_connection(to_server).await?;
        
        // Send transfer initiation message to current server
        let transfer_message = ProxyMessage {
            message_type: MessageType::ServerTransfer,
            player_id: Some(player_id),
            data: bincode::serialize(&to_server)?,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        
        // Notify current server about the transfer
        if let Some(server_conn) = self.server_connections.get(&from_server) {
            let serialized = bincode::serialize(&transfer_message)?;
            server_conn.sender.send(serialized)
                .map_err(|e| anyhow!("Failed to send transfer message to source server: {}", e))?;
        }
        
        // Update connection to point to new server
        if let Some(mut connection) = self.connections.get_mut(&client_id) {
            connection.target_server = Some(to_server);
            connection.current_server = Some(to_server);  // Switch immediately for new messages
        }
        
        info!("Server transfer initiated successfully for player {}", player_id);
        
        Ok(())
    }
    
    async fn forward_to_game_server(&self, client_id: Uuid, message: ProxyMessage) -> Result<()> {
        let connection = self.connections.get(&client_id)
            .ok_or_else(|| anyhow!("Connection not found for client {}", client_id))?;
        
        let server_id = connection.current_server
            .ok_or_else(|| anyhow!("No current server for client {}", client_id))?;
        
        debug!("Forwarding {:?} message to server {}", message.message_type, server_id);
        
        if let Some(server_conn) = self.server_connections.get(&server_id) {
            let serialized = bincode::serialize(&message)?;
            
            server_conn.sender.send(serialized)
                .map_err(|e| anyhow!("Failed to send to server {}: {}", server_id, e))?;
                
            debug!("Message forwarded successfully to server {}", server_id);
        } else {
            error!("Server connection not found for server {}", server_id);
            // Try to establish connection if missing
            self.ensure_server_connection(server_id).await?;
            return Err(anyhow!("Server connection not available: {}", server_id));
        }
        
        Ok(())
    }
    
    async fn ensure_server_connection(&self, server_id: Uuid) -> Result<()> {
        if self.server_connections.contains_key(&server_id) {
            debug!("Server connection already exists for {}", server_id);
            return Ok(());
        }
        
        info!("Establishing new connection to server {}", server_id);
        
        let server_info = self.game_server_manager
            .get_server(server_id)
            .await
            .ok_or_else(|| anyhow!("Server info not found for {}", server_id))?;
        
        let url = format!("ws://{}:{}", server_info.address, server_info.port);
        info!("Connecting to game server {} at {}", server_id, url);
        
        let (ws_stream, _) = connect_async(&url).await
            .map_err(|e| anyhow!("Failed to connect to server {}: {}", server_id, e))?;
        
        let (mut sender, mut receiver) = ws_stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        
        let server_connection = ServerConnection {
            sender: tx,
            server_id,
        };
        
        self.server_connections.insert(server_id, server_connection);
        
        let server_id_clone = server_id;
        
        // Spawn task to handle outgoing messages to server
        tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                if let Err(e) = sender.send(WsMessage::Binary(data)).await {
                    error!("Failed to send to server {}: {}", server_id_clone, e);
                    break;
                }
            }
            info!("Outgoing connection to server {} closed", server_id_clone);
        });
        
        // Spawn task to handle incoming messages from server
        let _proxy_connections = Arc::clone(&self.connections);
        tokio::spawn(async move {
            while let Some(msg) = receiver.next().await {
                match msg {
                    Ok(WsMessage::Binary(data)) => {
                        debug!("Received {} bytes from server {}", data.len(), server_id_clone);
                        // TODO: Process incoming server messages and route to appropriate clients
                        // This would include state updates, transfer confirmations, etc.
                    }
                    Ok(WsMessage::Text(text)) => {
                        debug!("Received text message from server {}: {}", server_id_clone, text);
                    }
                    Err(e) => {
                        error!("Server connection error for {}: {}", server_id_clone, e);
                        break;
                    }
                    _ => {}
                }
            }
            info!("Incoming connection from server {} closed", server_id_clone);
        });
        
        info!("Server connection established successfully for {}", server_id);
        
        Ok(())
    }
    
    async fn handle_heartbeat(&self, client_id: Uuid) -> Result<()> {
        debug!("Heartbeat from client: {}", client_id);
        Ok(())
    }
    
    async fn cleanup_client_connection(&self, client_id: Uuid) {
        info!("Cleaning up client connection: {}", client_id);
        
        if let Some(connection) = self.connections.remove(&client_id) {
            if let Some(player_id) = connection.1.player_id {
                info!("Removing player {} from tracking", player_id);
                
                // Remove any active transitions for this player
                self.active_transitions.remove(&player_id);
                
                // TODO: Notify game server about player disconnect
                if let Some(server_id) = connection.1.current_server {
                    let disconnect_message = ProxyMessage {
                        message_type: MessageType::PlayerDisconnect,
                        player_id: Some(player_id),
                        data: Vec::new(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    };
                    
                    if let Some(server_conn) = self.server_connections.get(&server_id) {
                        if let Ok(serialized) = bincode::serialize(&disconnect_message) {
                            let _ = server_conn.sender.send(serialized);
                        }
                    }
                }
            }
        }
    }
    
    pub async fn get_connection_count(&self) -> usize {
        self.connections.len()
    }
    
    pub async fn get_active_transitions(&self) -> Vec<ServerTransition> {
        self.active_transitions.iter().map(|t| t.value().clone()).collect()
    }
}