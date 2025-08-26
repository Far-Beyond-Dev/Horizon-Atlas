use crate::config::ProxyConfig;
use crate::errors::{AtlasError, Result};
use crate::types::{ClientId, ServerId, Message, MessageSource, MessageDestination, ClientConnection};
use crate::crypto::CryptoManager;
use serde::{Serialize, Deserialize};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, atomic::{AtomicU32, Ordering}};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{timeout, interval};
use tokio_tungstenite::{accept_async, connect_async, WebSocketStream, MaybeTlsStream};
use tokio_tungstenite::tungstenite::{Message as WsMessage, protocol::CloseFrame};
use tracing::{debug, error, info, warn, instrument};
use uuid::Uuid;

pub type WsClientStream = WebSocketStream<TcpStream>;
pub type WsServerStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[async_trait]
pub trait MessageHandler: Send + Sync {
    async fn handle_client_message(&self, client_id: &ClientId, message: Message) -> Result<()>;
    async fn handle_server_message(&self, server_id: &ServerId, message: Message) -> Result<()>;
    async fn route_message(&self, message: Message) -> Result<()>;
}

pub struct ProxyManager {
    config: ProxyConfig,
    crypto_manager: Arc<CryptoManager>,
    client_connections: Arc<DashMap<ClientId, ClientConnectionInfo>>,
    server_connections: Arc<DashMap<ServerId, ServerConnectionInfo>>,
    connection_pools: Arc<DashMap<ServerId, ServerConnectionPool>>,
    message_handler: Arc<dyn MessageHandler>,
    metrics: ProxyMetrics,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

#[derive(Debug, Clone)]
struct ClientConnectionInfo {
    connection: ClientConnection,
    sender: mpsc::UnboundedSender<WsMessage>,
    last_activity: std::time::Instant,
}

#[derive(Debug)]
struct ServerConnectionInfo {
    server_id: ServerId,
    address: SocketAddr,
    sender: mpsc::UnboundedSender<WsMessage>,
    receiver: Arc<Mutex<mpsc::UnboundedReceiver<Message>>>,
    last_activity: std::time::Instant,
    connection_count: AtomicU32,
}

#[derive(Debug)]
struct ServerConnectionPool {
    server_id: ServerId,
    address: SocketAddr,
    active_connections: Arc<DashMap<u32, ServerConnectionHandle>>,
    connection_counter: AtomicU32,
    max_connections: u32,
}

#[derive(Debug)]
struct ServerConnectionHandle {
    id: u32,
    sender: mpsc::UnboundedSender<WsMessage>,
    last_used: Arc<RwLock<std::time::Instant>>,
}

#[derive(Debug, Default)]
struct ProxyMetrics {
    active_client_connections: AtomicU32,
    active_server_connections: AtomicU32,
    messages_processed: AtomicU32,
    messages_failed: AtomicU32,
    bytes_transferred: AtomicU32,
    connection_errors: AtomicU32,
}

impl ProxyManager {
    pub fn new(
        config: ProxyConfig,
        crypto_manager: Arc<CryptoManager>,
        message_handler: Arc<dyn MessageHandler>,
    ) -> Self {
        Self {
            config,
            crypto_manager,
            client_connections: Arc::new(DashMap::new()),
            server_connections: Arc::new(DashMap::new()),
            connection_pools: Arc::new(DashMap::new()),
            message_handler,
            metrics: ProxyMetrics::default(),
            shutdown_tx: None,
        }
    }
    
    pub async fn start(&mut self, bind_addr: SocketAddr) -> Result<()> {
        info!("Starting proxy manager on {}", bind_addr);
        
        let listener = TcpListener::bind(bind_addr).await
            .map_err(|e| AtlasError::Network(e))?;
        
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);
        
        let client_connections = Arc::clone(&self.client_connections);
        let server_connections = Arc::clone(&self.server_connections);
        let crypto_manager = Arc::clone(&self.crypto_manager);
        let message_handler = Arc::clone(&self.message_handler);
        let config = self.config.clone();
        let metrics = self.metrics.clone();
        
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, addr)) => {
                                let client_connections = Arc::clone(&client_connections);
                                let crypto_manager = Arc::clone(&crypto_manager);
                                let message_handler = Arc::clone(&message_handler);
                                let config = config.clone();
                                let metrics = metrics.clone();
                                
                                tokio::spawn(async move {
                                    if let Err(e) = handle_client_connection(
                                        stream,
                                        addr,
                                        client_connections,
                                        crypto_manager,
                                        message_handler,
                                        config,
                                        metrics,
                                    ).await {
                                        error!("Error handling client connection from {}: {}", addr, e);
                                    }
                                });
                            }
                            Err(e) => {
                                error!("Failed to accept connection: {}", e);
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        info!("Proxy manager shutdown requested");
                        break;
                    }
                }
            }
        });
        
        self.start_cleanup_task().await;
        Ok(())
    }
    
    pub async fn stop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        
        for entry in self.client_connections.iter() {
            if let Err(e) = entry.value().sender.send(WsMessage::Close(None)) {
                debug!("Failed to send close message to client {}: {}", entry.key(), e);
            }
        }
        
        for entry in self.server_connections.iter() {
            if let Err(e) = entry.value().sender.send(WsMessage::Close(None)) {
                debug!("Failed to send close message to server {}: {}", entry.key(), e);
            }
        }
        
        self.client_connections.clear();
        self.server_connections.clear();
        self.connection_pools.clear();
        
        info!("Proxy manager stopped");
    }
    
    pub async fn send_to_client(&self, client_id: &ClientId, message: WsMessage) -> Result<()> {
        if let Some(conn) = self.client_connections.get(client_id) {
            conn.sender.send(message)
                .map_err(|_| AtlasError::ClientConnection("Failed to send message".to_string()))?;
            Ok(())
        } else {
            Err(AtlasError::ClientConnection(format!("Client {} not found", client_id.0)))
        }
    }
    
    pub async fn send_to_server(&self, server_id: &ServerId, message: WsMessage) -> Result<()> {
        if let Some(conn) = self.server_connections.get(server_id) {
            conn.sender.send(message)
                .map_err(|_| AtlasError::ServerConnection("Failed to send message".to_string()))?;
            Ok(())
        } else {
            Err(AtlasError::ServerConnection(format!("Server {} not found", server_id.0)))
        }
    }
    
    pub async fn connect_to_server(&self, server_id: ServerId, address: SocketAddr) -> Result<()> {
        if self.server_connections.contains_key(&server_id) {
            return Ok(());
        }
        
        info!("Connecting to server {} at {}", server_id, address);
        
        let ws_url = format!("ws://{}/ws", address);
        let (ws_stream, _) = connect_async(&ws_url).await
            .map_err(|e| AtlasError::ServerConnection(format!("Failed to connect to {}: {}", ws_url, e)))?;
        
        let (mut ws_sink, mut ws_stream) = ws_stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        
        let server_info = ServerConnectionInfo {
            server_id: server_id.clone(),
            address,
            sender: tx,
            receiver: Arc::new(Mutex::new(msg_rx)),
            last_activity: std::time::Instant::now(),
            connection_count: AtomicU32::new(1),
        };
        
        self.server_connections.insert(server_id.clone(), server_info);
        self.metrics.active_server_connections.fetch_add(1, Ordering::SeqCst);
        
        let server_id_clone = server_id.clone();
        let message_handler = Arc::clone(&self.message_handler);
        let crypto_manager = Arc::clone(&self.crypto_manager);
        let metrics = self.metrics.clone();
        
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(msg) => {
                                if let Err(e) = ws_sink.send(msg).await {
                                    error!("Failed to send message to server {}: {}", server_id_clone, e);
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    ws_msg = ws_stream.next() => {
                        match ws_msg {
                            Some(Ok(msg)) => {
                                if let Err(e) = handle_server_message(
                                    &server_id_clone,
                                    msg,
                                    &message_handler,
                                    &crypto_manager,
                                    &metrics,
                                ).await {
                                    error!("Error handling server message: {}", e);
                                }
                            }
                            Some(Err(e)) => {
                                error!("WebSocket error from server {}: {}", server_id_clone, e);
                                break;
                            }
                            None => {
                                info!("Server {} disconnected", server_id_clone);
                                break;
                            }
                        }
                    }
                }
            }
        });
        
        Ok(())
    }
    
    async fn start_cleanup_task(&self) {
        let client_connections = Arc::clone(&self.client_connections);
        let server_connections = Arc::clone(&self.server_connections);
        let connection_timeout = Duration::from_millis(self.config.connection_timeout);
        
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(60));
            
            loop {
                interval.tick().await;
                let now = std::time::Instant::now();
                
                client_connections.retain(|_, conn| {
                    if now.duration_since(conn.last_activity) > connection_timeout {
                        let _ = conn.sender.send(WsMessage::Close(Some(CloseFrame {
                            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Going,
                            reason: "Connection timeout".into(),
                        })));
                        false
                    } else {
                        true
                    }
                });
                
                server_connections.retain(|_, conn| {
                    now.duration_since(conn.last_activity) <= connection_timeout
                });
            }
        });
    }
    
    pub fn get_metrics(&self) -> ProxyMetricsSnapshot {
        ProxyMetricsSnapshot {
            active_client_connections: self.metrics.active_client_connections.load(Ordering::SeqCst),
            active_server_connections: self.metrics.active_server_connections.load(Ordering::SeqCst),
            messages_processed: self.metrics.messages_processed.load(Ordering::SeqCst),
            messages_failed: self.metrics.messages_failed.load(Ordering::SeqCst),
            bytes_transferred: self.metrics.bytes_transferred.load(Ordering::SeqCst),
            connection_errors: self.metrics.connection_errors.load(Ordering::SeqCst),
        }
    }
}

impl Clone for ProxyMetrics {
    fn clone(&self) -> Self {
        Self {
            active_client_connections: AtomicU32::new(self.active_client_connections.load(Ordering::SeqCst)),
            active_server_connections: AtomicU32::new(self.active_server_connections.load(Ordering::SeqCst)),
            messages_processed: AtomicU32::new(self.messages_processed.load(Ordering::SeqCst)),
            messages_failed: AtomicU32::new(self.messages_failed.load(Ordering::SeqCst)),
            bytes_transferred: AtomicU32::new(self.bytes_transferred.load(Ordering::SeqCst)),
            connection_errors: AtomicU32::new(self.connection_errors.load(Ordering::SeqCst)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyMetricsSnapshot {
    pub active_client_connections: u32,
    pub active_server_connections: u32,
    pub messages_processed: u32,
    pub messages_failed: u32,
    pub bytes_transferred: u32,
    pub connection_errors: u32,
}

#[instrument(skip_all, fields(client_addr = %addr))]
async fn handle_client_connection(
    stream: TcpStream,
    addr: SocketAddr,
    client_connections: Arc<DashMap<ClientId, ClientConnectionInfo>>,
    crypto_manager: Arc<CryptoManager>,
    message_handler: Arc<dyn MessageHandler>,
    config: ProxyConfig,
    metrics: ProxyMetrics,
) -> Result<()> {
    let ws_stream = timeout(
        Duration::from_millis(config.connection_timeout),
        accept_async(stream),
    ).await
    .map_err(|_| AtlasError::ClientConnection("Connection timeout".to_string()))?
    .map_err(|e| AtlasError::WebSocket(e))?;
    
    let client_id = ClientId::new();
    let (mut ws_sink, mut ws_stream) = ws_stream.split();
    let (tx, mut rx) = mpsc::unbounded_channel();
    
    let client_connection = ClientConnection {
        id: client_id.clone(),
        server_id: None,
        current_region: None,
        position: None,
        connected_at: Utc::now(),
        last_activity: Utc::now(),
        metadata: HashMap::new(),
    };
    
    let client_info = ClientConnectionInfo {
        connection: client_connection,
        sender: tx,
        last_activity: std::time::Instant::now(),
    };
    
    client_connections.insert(client_id.clone(), client_info);
    metrics.active_client_connections.fetch_add(1, Ordering::SeqCst);
    
    info!("Client {} connected from {}", client_id.0, addr);
    
    let client_id_clone = client_id.clone();
    let client_connections_clone = Arc::clone(&client_connections);
    let message_handler_clone = Arc::clone(&message_handler);
    let crypto_manager_clone = Arc::clone(&crypto_manager);
    let metrics_clone = metrics.clone();
    
    let outgoing_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = ws_sink.send(msg).await {
                error!("Failed to send message to client {}: {}", client_id_clone.0, e);
                break;
            }
        }
    });
    
    loop {
        tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(ws_msg)) => {
                        if let Some(mut conn_info) = client_connections.get_mut(&client_id) {
                            conn_info.last_activity = std::time::Instant::now();
                            conn_info.connection.last_activity = Utc::now();
                        }
                        
                        if let Err(e) = handle_client_message(
                            &client_id,
                            ws_msg,
                            &message_handler,
                            &crypto_manager,
                            &metrics,
                        ).await {
                            error!("Error handling client message: {}", e);
                            metrics.messages_failed.fetch_add(1, Ordering::SeqCst);
                        } else {
                            metrics.messages_processed.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error from client {}: {}", client_id.0, e);
                        metrics.connection_errors.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                    None => {
                        info!("Client {} disconnected", client_id.0);
                        break;
                    }
                }
            }
            _ = outgoing_task => {
                debug!("Outgoing task completed for client {}", client_id.0);
                break;
            }
        }
    }
    
    client_connections.remove(&client_id);
    metrics.active_client_connections.fetch_sub(1, Ordering::SeqCst);
    
    Ok(())
}

async fn handle_client_message(
    client_id: &ClientId,
    ws_msg: WsMessage,
    message_handler: &Arc<dyn MessageHandler>,
    crypto_manager: &Arc<CryptoManager>,
    metrics: &ProxyMetrics,
) -> Result<()> {
    let payload = match ws_msg {
        WsMessage::Binary(data) => data,
        WsMessage::Text(text) => text.into_bytes(),
        WsMessage::Close(_) => return Ok(()),
        WsMessage::Ping(data) => {
            return Ok(());
        }
        WsMessage::Pong(_) => return Ok(()),
        _ => return Ok(()),
    };
    
    metrics.bytes_transferred.fetch_add(payload.len() as u32, Ordering::SeqCst);
    
    let decrypted_payload = if crypto_manager.is_encryption_enabled() {
        crypto_manager.decrypt(&payload).await?
    } else {
        payload
    };
    
    let decompressed_payload = decompress_if_needed(&decrypted_payload)?;
    
    let message: Message = bincode::deserialize(&decompressed_payload)
        .map_err(|e| AtlasError::Serialization(e))?;
    
    message_handler.handle_client_message(client_id, message).await
}

async fn handle_server_message(
    server_id: &ServerId,
    ws_msg: WsMessage,
    message_handler: &Arc<dyn MessageHandler>,
    crypto_manager: &Arc<CryptoManager>,
    metrics: &ProxyMetrics,
) -> Result<()> {
    let payload = match ws_msg {
        WsMessage::Binary(data) => data,
        WsMessage::Text(text) => text.into_bytes(),
        WsMessage::Close(_) => return Ok(()),
        WsMessage::Ping(_) | WsMessage::Pong(_) => return Ok(()),
        _ => return Ok(()),
    };
    
    metrics.bytes_transferred.fetch_add(payload.len() as u32, Ordering::SeqCst);
    
    let decrypted_payload = if crypto_manager.is_encryption_enabled() {
        crypto_manager.decrypt(&payload).await?
    } else {
        payload
    };
    
    let decompressed_payload = decompress_if_needed(&decrypted_payload)?;
    
    let message: Message = bincode::deserialize(&decompressed_payload)
        .map_err(|e| AtlasError::Serialization(e))?;
    
    message_handler.handle_server_message(server_id, message).await
}

fn compress_if_needed(data: &[u8], config: &ProxyConfig) -> Result<Vec<u8>> {
    if !config.compression.enabled || data.len() < config.compression.min_size as usize {
        return Ok(data.to_vec());
    }
    
    match config.compression.algorithm {
        crate::config::CompressionAlgorithm::Lz4 => {
            lz4_flex::compress_prepend_size(data)
                .map_err(|e| AtlasError::Internal(format!("Compression failed: {}", e)))
        }
        _ => Ok(data.to_vec()),
    }
}

fn decompress_if_needed(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 4 {
        return Ok(data.to_vec());
    }
    
    if let Ok(decompressed) = lz4_flex::decompress_size_prepended(data) {
        Ok(decompressed)
    } else {
        Ok(data.to_vec())
    }
}