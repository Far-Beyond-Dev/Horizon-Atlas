use std::sync::mpsc;
use std::thread;
use std::sync::Arc;
use serde_json::Value;
use crate::error::Result;
use crate::spatial::WorldCoordinate;
use crate::persistence::PlayerPersistence;

/// Data skimming functionality for analyzing traffic without blocking passthrough
pub struct DataSkimmer {
    tx: mpsc::Sender<SkimData>,
    persistence: Arc<PlayerPersistence>,
}

/// Data structure for skimming operations
#[derive(Debug, Clone)]
pub struct SkimData {
    pub client_id: String,
    pub data: Vec<u8>,
    pub direction: DataDirection,
    pub timestamp: std::time::SystemTime,
}

#[derive(Debug, Clone)]
pub enum DataDirection {
    ClientToServer,
    ServerToClient,
}

impl DataSkimmer {
    /// Create a new data skimmer with a background processing thread
    pub fn new(persistence: Arc<PlayerPersistence>) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let persistence_worker = persistence.clone();
        
        thread::spawn(move || {
            Self::skim_worker(rx, persistence_worker);
        });
        
        Ok(Self { tx, persistence })
    }
    
    /// Send data for skimming (non-blocking)
    pub fn skim(&self, client_id: String, data: Vec<u8>, direction: DataDirection) {
        let skim_data = SkimData {
            client_id,
            data,
            direction,
            timestamp: std::time::SystemTime::now(),
        };
        
        // Non-blocking send - if channel is full, data is dropped
        let _ = self.tx.send(skim_data);
    }
    
    /// Background worker thread for processing skimmed data
    fn skim_worker(rx: mpsc::Receiver<SkimData>, persistence: Arc<PlayerPersistence>) {
        for skim_data in rx {
            Self::process_data(&skim_data, &persistence);
        }
    }
    
    /// Process individual data packets
    fn process_data(skim_data: &SkimData, persistence: &PlayerPersistence) {
        if let Ok(text) = std::str::from_utf8(&skim_data.data) {
            // Process each line/message separately
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                
                // Try to parse as JSON
                if let Ok(json) = serde_json::from_str::<Value>(line) {
                    Self::process_json_message(&json, &skim_data.client_id, &skim_data.direction, persistence);
                }
            }
            
            // Look for client movement commands (legacy text-based)
            if Self::contains_client_move(text) {
                println!(
                    "[{}] Client {} movement detected: {}",
                    skim_data.timestamp.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    skim_data.client_id,
                    Self::extract_move_info(text)
                );
            }
            
            // Look for other important game events
            Self::analyze_game_events(text, &skim_data.client_id);
        }
    }
    
    /// Check if data contains client movement commands
    fn contains_client_move(text: &str) -> bool {
        text.contains("\"client\":\"move\"") || 
        text.contains("\"type\":\"move\"") ||
        text.contains("\"action\":\"move\"")
    }
    
    /// Extract movement information from text
    fn extract_move_info(text: &str) -> String {
        for part in text.split(|c| c == '\n' || c == '\r') {
            let part = part.trim();
            if !part.is_empty() && Self::contains_client_move(part) {
                return part.to_string();
            }
        }
        "Movement data".to_string()
    }
    
    /// Analyze various game events for insights
    fn analyze_game_events(text: &str, client_id: &str) {
        // Look for connection events
        if text.contains("\"type\":\"connect\"") || text.contains("\"event\":\"join\"") {
            println!("[GAME] Client {} connected", client_id);
        }
        
        // Look for disconnect events
        if text.contains("\"type\":\"disconnect\"") || text.contains("\"event\":\"leave\"") {
            println!("[GAME] Client {} disconnected", client_id);
        }
        
        // Look for error conditions
        if text.contains("\"error\"") || text.contains("\"status\":\"error\"") {
            println!("[GAME] Error detected for client {}: {}", client_id, text);
        }
    }
}

    /// Process JSON message for position tracking and game events
    fn process_json_message(json: &Value, client_id: &str, direction: &DataDirection, persistence: &PlayerPersistence) {
        match direction {
            DataDirection::ClientToServer => {
                Self::process_client_to_server_message(json, client_id, persistence);
            }
            DataDirection::ServerToClient => {
                Self::process_server_to_client_message(json, client_id, persistence);
            }
        }
    }
    
    /// Process messages from client to server
    fn process_client_to_server_message(json: &Value, client_id: &str, persistence: &PlayerPersistence) {
        // Extract message type
        let msg_type = json.get("type").and_then(|v| v.as_str());
        
        match msg_type {
            Some("move") | Some("position") => {
                if let Some(position) = Self::extract_position_from_json(json) {
                    // Update player position in persistence
                    if let Err(e) = persistence.update_player_position(client_id, position) {
                        println!("[SKIM] Failed to update position for {}: {}", client_id, e);
                    }
                    
                    println!("[POSITION] Client {} moved to ({:.2}, {:.2}, {:.2})", 
                            client_id, position.x, position.y, position.z);
                }
            }
            Some("join") | Some("connect") => {
                // Player joining - extract initial position if available
                if let Some(position) = Self::extract_position_from_json(json) {
                    println!("[GAME] Client {} joining at position ({:.2}, {:.2}, {:.2})", 
                            client_id, position.x, position.y, position.z);
                }
            }
            Some("chat") => {
                if let Some(message) = json.get("message").and_then(|v| v.as_str()) {
                    println!("[CHAT] Client {}: {}", client_id, message);
                }
            }
            Some("action") => {
                if let Some(action) = json.get("action").and_then(|v| v.as_str()) {
                    println!("[ACTION] Client {} performed action: {}", client_id, action);
                }
            }
            _ => {
                // Check for legacy format
                if json.get("client").and_then(|v| v.as_str()) == Some("move") {
                    if let Some(position) = Self::extract_position_from_json(json) {
                        if let Err(e) = persistence.update_player_position(client_id, position) {
                            println!("[SKIM] Failed to update position for {}: {}", client_id, e);
                        }
                        
                        println!("[POSITION] Client {} moved to ({:.2}, {:.2}, {:.2})", 
                                client_id, position.x, position.y, position.z);
                    }
                }
            }
        }
    }
    
    /// Process messages from server to client
    fn process_server_to_client_message(json: &Value, client_id: &str, _persistence: &PlayerPersistence) {
        let msg_type = json.get("type").and_then(|v| v.as_str());
        
        match msg_type {
            Some("world_update") => {
                // Server sending world state updates
                if let Some(players) = json.get("players").and_then(|v| v.as_array()) {
                    println!("[WORLD] Server sending world update with {} players to {}", 
                            players.len(), client_id);
                }
            }
            Some("transfer_prepare") => {
                // Server preparing client for transfer
                println!("[TRANSFER] Server preparing client {} for transfer", client_id);
            }
            Some("error") => {
                if let Some(error_msg) = json.get("message").and_then(|v| v.as_str()) {
                    println!("[ERROR] Server error for client {}: {}", client_id, error_msg);
                }
            }
            _ => {}
        }
    }
    
    /// Extract world coordinates from JSON message
    fn extract_position_from_json(json: &Value) -> Option<WorldCoordinate> {
        // Try different position formats
        
        // Format 1: {x, y, z} at root level
        if let (Some(x), Some(y), Some(z)) = (
            json.get("x").and_then(|v| v.as_f64()),
            json.get("y").and_then(|v| v.as_f64()),
            json.get("z").and_then(|v| v.as_f64())
        ) {
            return Some(WorldCoordinate::new(x, y, z));
        }
        
        // Format 2: {position: {x, y, z}}
        if let Some(pos_obj) = json.get("position") {
            if let (Some(x), Some(y), Some(z)) = (
                pos_obj.get("x").and_then(|v| v.as_f64()),
                pos_obj.get("y").and_then(|v| v.as_f64()),
                pos_obj.get("z").and_then(|v| v.as_f64())
            ) {
                return Some(WorldCoordinate::new(x, y, z));
            }
        }
        
        // Format 3: {pos: [x, y, z]}
        if let Some(pos_array) = json.get("pos").and_then(|v| v.as_array()) {
            if pos_array.len() >= 3 {
                if let (Some(x), Some(y), Some(z)) = (
                    pos_array[0].as_f64(),
                    pos_array[1].as_f64(),
                    pos_array[2].as_f64()
                ) {
                    return Some(WorldCoordinate::new(x, y, z));
                }
            }
        }
        
        // Format 4: {coords: {x, y, z}}
        if let Some(coords_obj) = json.get("coords") {
            if let (Some(x), Some(y), Some(z)) = (
                coords_obj.get("x").and_then(|v| v.as_f64()),
                coords_obj.get("y").and_then(|v| v.as_f64()),
                coords_obj.get("z").and_then(|v| v.as_f64())
            ) {
                return Some(WorldCoordinate::new(x, y, z));
            }
        }
        
        None
    }
    
    /// Create message to inject into client/server stream
    pub fn create_transfer_notification_message(&self, client_id: &str, from_server: &str, to_server: &str) -> Vec<u8> {
        let message = serde_json::json!({
            "type": "transfer_notification",
            "client_id": client_id,
            "from_server": from_server,
            "to_server": to_server,
            "timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        });
        
        format!("{}\n", message.to_string()).into_bytes()
    }
    
    /// Create message to prepare server for incoming client
    pub fn create_server_prepare_message(&self, client_id: &str, player_data: &crate::persistence::PlayerData) -> Vec<u8> {
        let message = serde_json::json!({
            "type": "client_transfer_incoming",
            "client_id": client_id,
            "position": {
                "x": player_data.last_position.x,
                "y": player_data.last_position.y,
                "z": player_data.last_position.z
            },
            "velocity": {
                "x": player_data.movement_data.velocity.x,
                "y": player_data.movement_data.velocity.y,
                "z": player_data.movement_data.velocity.z
            },
            "session_data": player_data.session_data,
            "timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        });
        
        format!("{}\n", message.to_string()).into_bytes()
    }
}