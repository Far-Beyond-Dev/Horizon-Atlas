use crate::state::{ConnectionInfo, PlayerState};
use crate::game_server::GameServerManager;
use crate::regions::{RegionManager, RegionTransition};
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
    server_connections: Arc<DashMap<String, ServerConnection>>,
    game_server_manager: Arc<GameServerManager>,
    region_manager: Arc<RegionManager>,
    encryption_manager: Arc<EncryptionManager>,
    compressor: Arc<DeltaCompressor>,
    active_transitions: Arc<DashMap<Uuid, RegionTransition>>,
}

struct ServerConnection {
    sender: mpsc::UnboundedSender<Vec<u8>>,
    server_id: String,
}

impl WebSocketProxy {
    pub fn new(
        game_server_manager: Arc<GameServerManager>,
        region_manager: Arc<RegionManager>,
        encryption_manager: Arc<EncryptionManager>,
        compressor: Arc<DeltaCompressor>,
    ) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            server_connections: Arc::new(DashMap::new()),
            game_server_manager,
            region_manager,
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
        
        let server_id = self.region_manager
            .find_server_for_position(&player_state.position)
            .await
            .unwrap_or_else(|| "default-server".to_string());
        
        if let Some(mut connection) = self.connections.get_mut(&client_id) {
            connection.player_id = Some(player_id);
            connection.current_server = Some(server_id.clone());
        }
        
        self.ensure_server_connection(&server_id).await?;
        self.forward_to_game_server(client_id, message).await?;
        
        Ok(())
    }
    
    async fn handle_player_move(&self, client_id: Uuid, message: ProxyMessage) -> Result<()> {
        let player_state: PlayerState = bincode::deserialize(&message.data)?;
        let player_id = player_state.id;
        
        self.region_manager.update_player_position(player_id, player_state.position.clone()).await?;
        
        let connection = self.connections.get(&client_id)
            .ok_or_else(|| anyhow!("Connection not found"))?;
        
        if let Some(_current_server) = &connection.current_server {
            if let Some(new_region) = self.region_manager
                .should_transfer_player(&player_state.position, &player_state.region_id)
                .await 
            {
                self.initiate_server_transfer(client_id, player_id, new_region).await?;
            }
        }
        
        self.forward_to_game_server(client_id, message).await?;
        Ok(())
    }
    
    async fn initiate_server_transfer(&self, client_id: Uuid, player_id: Uuid, new_region: String) -> Result<()> {
        let connection = self.connections.get(&client_id)
            .ok_or_else(|| anyhow!("Connection not found"))?;
        
        let current_server = connection.current_server.as_ref()
            .ok_or_else(|| anyhow!("No current server"))?;
        
        let new_server = self.region_manager
            .find_server_for_position(&crate::state::Vector3::new(0.0, 0.0, 0.0))
            .await
            .ok_or_else(|| anyhow!("No server found for new region"))?;
        
        if current_server == &new_server {
            return Ok(());
        }
        
        info!("Initiating server transfer for player {} from {} to {}", 
              player_id, current_server, new_server);
        
        let transition = RegionTransition::new(
            player_id,
            connection.current_server.clone().unwrap_or_default(),
            new_region,
            current_server.clone(),
            new_server.clone(),
        );
        
        self.active_transitions.insert(player_id, transition);
        self.ensure_server_connection(&new_server).await?;
        
        Ok(())
    }
    
    async fn forward_to_game_server(&self, client_id: Uuid, message: ProxyMessage) -> Result<()> {
        let connection = self.connections.get(&client_id)
            .ok_or_else(|| anyhow!("Connection not found"))?;
        
        let server_id = connection.current_server.as_ref()
            .ok_or_else(|| anyhow!("No current server"))?;
        
        if let Some(server_conn) = self.server_connections.get(server_id) {
            let serialized = bincode::serialize(&message)?;
            
            server_conn.sender.send(serialized)
                .map_err(|e| anyhow!("Failed to send to server: {}", e))?;
        } else {
            warn!("Server connection not found: {}", server_id);
        }
        
        Ok(())
    }
    
    async fn ensure_server_connection(&self, server_id: &str) -> Result<()> {
        if self.server_connections.contains_key(server_id) {
            return Ok(());
        }
        
        let server_info = self.game_server_manager
            .get_server(server_id)
            .await
            .ok_or_else(|| anyhow!("Server info not found"))?;
        
        let url = format!("ws://{}:{}", server_info.address, server_info.port);
        info!("Connecting to game server: {}", url);
        
        let (ws_stream, _) = connect_async(&url).await
            .map_err(|e| anyhow!("Failed to connect to server: {}", e))?;
        
        let (mut sender, mut receiver) = ws_stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        
        let server_connection = ServerConnection {
            sender: tx,
            server_id: server_id.to_string(),
        };
        
        self.server_connections.insert(server_id.to_string(), server_connection);
        
        let server_id_clone = server_id.to_string();
        
        tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                if let Err(e) = sender.send(WsMessage::Binary(data)).await {
                    error!("Failed to send to server {}: {}", server_id_clone, e);
                    break;
                }
            }
        });
        
        tokio::spawn(async move {
            while let Some(msg) = receiver.next().await {
                match msg {
                    Ok(WsMessage::Binary(_data)) => {
                        debug!("Received message from server");
                    }
                    Err(e) => {
                        error!("Server connection error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });
        
        Ok(())
    }
    
    async fn handle_heartbeat(&self, client_id: Uuid) -> Result<()> {
        debug!("Heartbeat from client: {}", client_id);
        Ok(())
    }
    
    async fn cleanup_client_connection(&self, client_id: Uuid) {
        info!("Cleaning up client connection: {}", client_id);
        self.connections.remove(&client_id);
    }
    
    pub async fn get_connection_count(&self) -> usize {
        self.connections.len()
    }
    
    pub async fn get_active_transitions(&self) -> Vec<RegionTransition> {
        self.active_transitions.iter().map(|t| t.value().clone()).collect()
    }
}