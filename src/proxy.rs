use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::collections::HashMap;

use crate::client::{ClientConnection, ClientState};
use crate::server::ServerManager;
use crate::transfer::TransferManager;
use crate::skim::{DataSkimmer, DataDirection};
use crate::config::ProxyConfig;
use crate::error::{ProxyError, Result};
use crate::persistence::{PlayerPersistence, PlayerData};
use crate::spatial::WorldCoordinate;

/// Main proxy server that handles client connections and server transfers
pub struct HorizonProxy {
    config: ProxyConfig,
    server_manager: Arc<ServerManager>,
    transfer_manager: Arc<TransferManager>,
    data_skimmer: Arc<DataSkimmer>,
    player_persistence: Arc<PlayerPersistence>,
    active_connections: Arc<Mutex<HashMap<String, Arc<ClientConnection>>>>,
}

impl HorizonProxy {
    /// Create a new Horizon Atlas proxy
    pub fn new(config: ProxyConfig) -> Result<Self> {
        let server_manager = Arc::new(ServerManager::new(config.servers.clone()));
        let transfer_manager = Arc::new(TransferManager::new(server_manager.clone()));
        
        // Initialize player persistence
        let player_persistence = Arc::new(PlayerPersistence::new(
            &config.spatial_config.player_data_file,
            config.spatial_config.auto_save_interval,
        )?);
        
        let data_skimmer = Arc::new(DataSkimmer::new(player_persistence.clone())?);
        let active_connections = Arc::new(Mutex::new(HashMap::new()));
        
        Ok(Self {
            config,
            server_manager,
            transfer_manager,
            data_skimmer,
            player_persistence,
            active_connections,
        })
    }
    
    /// Start the proxy server
    pub fn start(&self) -> Result<()> {
        let listener = TcpListener::bind(self.config.listen_addr)?;
        println!("Horizon Atlas Proxy listening on {}", self.config.listen_addr);
        
        // Start health check thread
        self.start_health_check_thread();
        
        // Start load balancing thread
        self.start_load_balancing_thread();
        
        for stream in listener.incoming() {
            match stream {
                Ok(client_stream) => {
                    let server_manager = self.server_manager.clone();
                    let transfer_manager = self.transfer_manager.clone();
                    let data_skimmer = self.data_skimmer.clone();
                    let active_connections = self.active_connections.clone();
                    let buffer_size = self.config.buffer_size;
                    
                    let config_clone = self.config.clone();
                    let player_persistence_clone = self.player_persistence.clone();
                    
                    thread::spawn(move || {
                        if let Err(e) = Self::handle_client(
                            client_stream,
                            server_manager,
                            transfer_manager,
                            data_skimmer,
                            active_connections,
                            buffer_size,
                            config_clone,
                            player_persistence_clone,
                        ) {
                            println!("[ERROR] Client handling failed: {}", e);
                        }
                    });
                }
                Err(e) => {
                    println!("[ERROR] Failed to accept client connection: {}", e);
                }
            }
        }
        
        Ok(())
    }
    
    /// Handle individual client connections
    fn handle_client(
        client_stream: TcpStream,
        server_manager: Arc<ServerManager>,
        _transfer_manager: Arc<TransferManager>,
        data_skimmer: Arc<DataSkimmer>,
        active_connections: Arc<Mutex<HashMap<String, Arc<ClientConnection>>>>,
        buffer_size: usize,
        config: ProxyConfig,
        player_persistence: Arc<PlayerPersistence>,
    ) -> Result<()> {
        // Use spatial routing to find the best server for this client
        let server_config = Self::find_server_for_client(
            &client_stream,
            &config,
            &player_persistence,
        )?;
            
        // Create client connection wrapper
        let client = Arc::new(ClientConnection::new(client_stream, server_config.id.clone())?);
        let client_id = client.client_id();
        
        // Register client
        {
            let mut connections = active_connections.lock().unwrap();
            connections.insert(client_id.clone(), client.clone());
        }
        
        // Increment server connection count
        server_manager.increment_connections(&server_config.id)?;
        
        println!("[PROXY] Client {} connected, assigned to server {}", client_id, server_config.id);
        
        // Perform handshake and establish connection
        let result = Self::proxy_connection(
            client.clone(),
            server_manager.clone(),
            _transfer_manager,
            data_skimmer,
            buffer_size,
        );
        
        // Cleanup on disconnect
        server_manager.decrement_connections(&server_config.id)?;
        client.set_state(ClientState::Disconnected);
        
        {
            let mut connections = active_connections.lock().unwrap();
            connections.remove(&client_id);
        }
        
        println!("[PROXY] Client {} disconnected", client_id);
        
        result
    }
    
    /// Main proxy connection logic with handshake and bidirectional data flow
    fn proxy_connection(
        client: Arc<ClientConnection>,
        server_manager: Arc<ServerManager>,
        _transfer_manager: Arc<TransferManager>,
        data_skimmer: Arc<DataSkimmer>,
        buffer_size: usize,
    ) -> Result<()> {
        let client_id = client.client_id();
        let server_id = client.current_server();
        
        // Initial server connection
        let mut server_stream = server_manager.connect_to_server(&server_id)?;
        
        // Perform WebSocket handshake
        Self::perform_handshake(&client, &mut server_stream, buffer_size)?;
        
        client.set_state(ClientState::Connected);
        println!("[PROXY] Handshake completed for client {}", client_id);
        
        // Clone streams for bidirectional communication
        let client_read = client.try_clone()?;
        let client_write = client.try_clone()?;
        let server_read = server_stream.try_clone()?;
        let server_write = server_stream.try_clone()?;
        
        // Client-to-server thread
        let client_id_c2s = client_id.clone();
        let data_skimmer_c2s = data_skimmer.clone();
        let client_for_bytes = client.clone();
        let c2s_handle = thread::spawn(move || {
            Self::forward_data(
                client_read,
                server_write,
                client_id_c2s,
                data_skimmer_c2s,
                DataDirection::ClientToServer,
                client_for_bytes,
                buffer_size,
            )
        });
        
        // Server-to-client thread
        let client_id_s2c = client_id.clone();
        let data_skimmer_s2c = data_skimmer.clone();
        let client_for_bytes_s2c = client.clone();
        let s2c_handle = thread::spawn(move || {
            Self::forward_data(
                server_read,
                client_write,
                client_id_s2c,
                data_skimmer_s2c,
                DataDirection::ServerToClient,
                client_for_bytes_s2c,
                buffer_size,
            )
        });
        
        // Wait for either thread to complete (indicating connection closed)
        let _ = c2s_handle.join();
        let _ = s2c_handle.join();
        
        Ok(())
    }
    
    /// Perform initial WebSocket handshake between client and server
    fn perform_handshake(
        client: &ClientConnection,
        server_stream: &mut TcpStream,
        buffer_size: usize,
    ) -> Result<()> {
        let mut client_stream = client.try_clone()?;
        let mut handshake_buf = vec![0u8; buffer_size];
        
        // Read client handshake
        let n = client_stream.read(&mut handshake_buf)?;
        if n == 0 {
            return Err(ProxyError::Connection("Client closed connection during handshake".to_string()));
        }
        
        // Forward to server
        server_stream.write_all(&handshake_buf[..n])?;
        
        // Read server response
        let server_response_n = server_stream.read(&mut handshake_buf)?;
        if server_response_n == 0 {
            return Err(ProxyError::Connection("Server closed connection during handshake".to_string()));
        }
        
        // Forward to client
        client_stream.write_all(&handshake_buf[..server_response_n])?;
        
        Ok(())
    }
    
    /// Forward data between streams with skimming
    fn forward_data<R: Read, W: Write>(
        mut reader: R,
        mut writer: W,
        client_id: String,
        data_skimmer: Arc<DataSkimmer>,
        direction: DataDirection,
        client: Arc<ClientConnection>,
        buffer_size: usize,
    ) -> Result<()> {
        let mut buffer = vec![0u8; buffer_size];
        
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break, // Connection closed
                Ok(n) => {
                    let data = &buffer[..n];
                    
                    // Skim data for analysis (non-blocking)
                    data_skimmer.skim(client_id.clone(), data.to_vec(), direction.clone());
                    
                    // Forward data immediately
                    if let Err(_) = writer.write_all(data) {
                        break;
                    }
                    
                    // Update byte counters
                    client.add_bytes_transferred(n as u64);
                }
                Err(_) => break,
            }
        }
        
        Ok(())
    }
    
    /// Start health check thread for monitoring server health
    fn start_health_check_thread(&self) {
        let server_manager = self.server_manager.clone();
        
        thread::spawn(move || {
            loop {
                let server_stats = server_manager.get_server_stats();
                
                for server in &server_stats {
                    server_manager.health_check(&server.id);
                }
                
                // Health check every 30 seconds
                thread::sleep(std::time::Duration::from_secs(30));
            }
        });
    }
    
    /// Start load balancing thread for automatic client transfers
    fn start_load_balancing_thread(&self) {
        let _transfer_manager = self.transfer_manager.clone();
        let active_connections = self.active_connections.clone();
        
        thread::spawn(move || {
            loop {
                // Check every 60 seconds for load balancing opportunities
                thread::sleep(std::time::Duration::from_secs(60));
                
                let connections: Vec<Arc<ClientConnection>> = {
                    let conn_map = active_connections.lock().unwrap();
                    conn_map.values().cloned().collect()
                };
                
                // For now, skip automatic load balancing as it requires more complex implementation
                // TODO: Implement proper client transfer logic
                if connections.len() > 0 {
                    println!("[LOAD_BALANCER] Managing {} active connections", connections.len());
                }
            }
        });
    }
    
    /// Find the best server for a new client connection using spatial routing
    fn find_server_for_client(
        client_stream: &TcpStream,
        config: &ProxyConfig,
        player_persistence: &PlayerPersistence,
    ) -> Result<crate::config::ServerConfig> {
        // Get client address to create a unique identifier
        let client_addr = client_stream.peer_addr()?;
        let client_id = format!("temp-{}-{}", client_addr.ip(), client_addr.port());
        
        // Check if we have existing player data
        if let Some(player_data) = player_persistence.get_player(&client_id) {
            // Use spatial routing based on last known position
            if let Some(region) = config.find_region_for_position(&player_data.last_position) {
                if let Some(server) = config.get_server_by_id(&region.server_id) {
                    println!("[SPATIAL] Client {} returning to region {} at position ({:.1}, {:.1}, {:.1})", 
                            client_id, region.server_id, 
                            player_data.last_position.x, 
                            player_data.last_position.y, 
                            player_data.last_position.z);
                    return Ok(server.clone());
                }
            }
        }
        
        // No existing data - assign to the center region (spawn point)
        let spawn_position = WorldCoordinate::new(0.0, 0.0, 0.0);
        if let Some(region) = config.find_region_for_position(&spawn_position) {
            if let Some(server) = config.get_server_by_id(&region.server_id) {
                // Create new player data
                let new_player = PlayerData::new(client_id.clone(), spawn_position, region.server_id.clone());
                player_persistence.update_player(new_player);
                
                println!("[SPATIAL] New client {} assigned to spawn region {} at center", 
                        client_id, region.server_id);
                return Ok(server.clone());
            }
        }
        
        // Fallback to first available server
        config.servers.iter()
            .find(|s| s.can_accept_connections())
            .cloned()
            .ok_or_else(|| ProxyError::Connection("No available servers".to_string()))
    }
    
    /// Get current proxy statistics
    pub fn get_stats(&self) -> ProxyStats {
        let active_count = self.active_connections.lock().unwrap().len();
        let server_stats = self.server_manager.get_server_stats();
        
        ProxyStats {
            active_connections: active_count,
            total_servers: server_stats.len(),
            healthy_servers: server_stats.iter().filter(|s| s.healthy).count(),
            server_details: server_stats,
        }
    }
}

/// Proxy statistics
#[derive(Debug)]
pub struct ProxyStats {
    pub active_connections: usize,
    pub total_servers: usize,
    pub healthy_servers: usize,
    pub server_details: Vec<crate::config::ServerConfig>,
}