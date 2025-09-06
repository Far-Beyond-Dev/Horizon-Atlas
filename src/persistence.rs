use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};

use crate::spatial::{WorldCoordinate, MovementData};
use crate::error::{ProxyError, Result};

/// Player data for persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerData {
    /// Player unique identifier
    pub player_id: String,
    /// Last known world position
    pub last_position: WorldCoordinate,
    /// Movement tracking data
    pub movement_data: MovementData,
    /// Last connected server ID
    pub last_server: String,
    /// Last update timestamp
    pub last_updated: u64,
    /// Player session data
    pub session_data: HashMap<String, serde_json::Value>,
}

impl PlayerData {
    pub fn new(player_id: String, initial_position: WorldCoordinate, server_id: String) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
            
        Self {
            player_id: player_id.clone(),
            last_position: initial_position,
            movement_data: MovementData::new(initial_position),
            last_server: server_id,
            last_updated: timestamp,
            session_data: HashMap::new(),
        }
    }
    
    /// Update player position and movement data
    pub fn update_position(&mut self, new_position: WorldCoordinate) {
        self.last_position = new_position;
        self.movement_data.update_position(new_position);
        self.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }
    
    /// Update last connected server
    pub fn update_server(&mut self, server_id: String) {
        self.last_server = server_id;
        self.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }
    
    /// Set session data value
    pub fn set_session_data(&mut self, key: String, value: serde_json::Value) {
        self.session_data.insert(key, value);
        self.last_updated = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }
    
    /// Get session data value
    pub fn get_session_data(&self, key: &str) -> Option<&serde_json::Value> {
        self.session_data.get(key)
    }
}

/// Player database for JSON persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerDatabase {
    /// All player records
    pub players: HashMap<String, PlayerData>,
    /// Database metadata
    pub metadata: DatabaseMetadata,
}

/// Database metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseMetadata {
    /// Schema version
    pub version: String,
    /// Last save timestamp
    pub last_saved: u64,
    /// Total player count
    pub player_count: usize,
    /// Database creation timestamp
    pub created: u64,
}

impl PlayerDatabase {
    pub fn new() -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
            
        Self {
            players: HashMap::new(),
            metadata: DatabaseMetadata {
                version: "1.0.0".to_string(),
                last_saved: timestamp,
                player_count: 0,
                created: timestamp,
            },
        }
    }
    
    /// Update metadata before saving
    fn update_metadata(&mut self) {
        self.metadata.last_saved = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.metadata.player_count = self.players.len();
    }
}

/// JSON-based player data persistence manager
pub struct PlayerPersistence {
    /// File path for player data
    file_path: String,
    /// In-memory cache of player data
    cache: Arc<Mutex<PlayerDatabase>>,
    /// Auto-save interval in seconds
    auto_save_interval: u64,
    /// Last save timestamp
    last_save: Arc<Mutex<u64>>,
}

impl PlayerPersistence {
    /// Create a new player persistence manager
    pub fn new(file_path: &str, auto_save_interval: u64) -> Result<Self> {
        let persistence = Self {
            file_path: file_path.to_string(),
            cache: Arc::new(Mutex::new(PlayerDatabase::new())),
            auto_save_interval,
            last_save: Arc::new(Mutex::new(0)),
        };
        
        // Try to load existing data
        if let Err(e) = persistence.load() {
            println!("[PERSISTENCE] Could not load existing player data: {}. Starting with empty database.", e);
        }
        
        // Start auto-save thread
        persistence.start_auto_save_thread();
        
        Ok(persistence)
    }
    
    /// Load player data from JSON file
    pub fn load(&self) -> Result<()> {
        if !Path::new(&self.file_path).exists() {
            println!("[PERSISTENCE] Player data file does not exist, starting fresh");
            return Ok(());
        }
        
        let file = File::open(&self.file_path)
            .map_err(|e| ProxyError::Config(format!("Failed to open player data file: {}", e)))?;
            
        let reader = BufReader::new(file);
        let database: PlayerDatabase = serde_json::from_reader(reader)
            .map_err(|e| ProxyError::Config(format!("Failed to parse player data: {}", e)))?;
        
        let mut cache = self.cache.lock().unwrap();
        *cache = database;
        
        println!("[PERSISTENCE] Loaded {} player records from {}", cache.players.len(), self.file_path);
        Ok(())
    }
    
    /// Save player data to JSON file
    pub fn save(&self) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        cache.update_metadata();
        
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.file_path)
            .map_err(|e| ProxyError::Config(format!("Failed to create player data file: {}", e)))?;
            
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, &*cache)
            .map_err(|e| ProxyError::Config(format!("Failed to write player data: {}", e)))?;
        
        // Update last save timestamp
        let mut last_save = self.last_save.lock().unwrap();
        *last_save = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
            
        println!("[PERSISTENCE] Saved {} player records to {}", cache.players.len(), self.file_path);
        Ok(())
    }
    
    /// Get player data by ID
    pub fn get_player(&self, player_id: &str) -> Option<PlayerData> {
        let cache = self.cache.lock().unwrap();
        cache.players.get(player_id).cloned()
    }
    
    /// Update or create player data
    pub fn update_player(&self, player_data: PlayerData) {
        let mut cache = self.cache.lock().unwrap();
        cache.players.insert(player_data.player_id.clone(), player_data);
    }
    
    /// Update player position
    pub fn update_player_position(&self, player_id: &str, position: WorldCoordinate) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        
        if let Some(player) = cache.players.get_mut(player_id) {
            player.update_position(position);
            Ok(())
        } else {
            Err(ProxyError::Client(format!("Player {} not found", player_id)))
        }
    }
    
    /// Update player server assignment
    pub fn update_player_server(&self, player_id: &str, server_id: &str) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        
        if let Some(player) = cache.players.get_mut(player_id) {
            player.update_server(server_id.to_string());
            Ok(())
        } else {
            Err(ProxyError::Client(format!("Player {} not found", player_id)))
        }
    }
    
    /// Get all players in a specific server region
    pub fn get_players_in_region(&self, server_id: &str) -> Vec<PlayerData> {
        let cache = self.cache.lock().unwrap();
        cache.players
            .values()
            .filter(|player| player.last_server == server_id)
            .cloned()
            .collect()
    }
    
    /// Get players near a specific coordinate
    pub fn get_players_near(&self, center: WorldCoordinate, radius: f64) -> Vec<PlayerData> {
        let cache = self.cache.lock().unwrap();
        cache.players
            .values()
            .filter(|player| player.last_position.distance_to(&center) <= radius)
            .cloned()
            .collect()
    }
    
    /// Remove player data
    pub fn remove_player(&self, player_id: &str) -> Option<PlayerData> {
        let mut cache = self.cache.lock().unwrap();
        cache.players.remove(player_id)
    }
    
    /// Get all player IDs
    pub fn get_all_player_ids(&self) -> Vec<String> {
        let cache = self.cache.lock().unwrap();
        cache.players.keys().cloned().collect()
    }
    
    /// Get database statistics
    pub fn get_stats(&self) -> (usize, u64, u64) {
        let cache = self.cache.lock().unwrap();
        (
            cache.players.len(),
            cache.metadata.created,
            cache.metadata.last_saved,
        )
    }
    
    /// Check if auto-save is needed
    fn should_auto_save(&self) -> bool {
        let last_save = *self.last_save.lock().unwrap();
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
            
        current_time - last_save >= self.auto_save_interval
    }
    
    /// Start auto-save background thread
    fn start_auto_save_thread(&self) {
        let cache = self.cache.clone();
        let last_save = self.last_save.clone();
        let file_path = self.file_path.clone();
        let interval = self.auto_save_interval;
        
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(interval));
                
                // Check if save is needed
                let should_save = {
                    let last = *last_save.lock().unwrap();
                    let current = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    current - last >= interval
                };
                
                if should_save {
                    // Create a temporary persistence instance for saving
                    let temp_persistence = PlayerPersistence {
                        file_path: file_path.clone(),
                        cache: cache.clone(),
                        auto_save_interval: interval,
                        last_save: last_save.clone(),
                    };
                    
                    if let Err(e) = temp_persistence.save() {
                        println!("[PERSISTENCE] Auto-save failed: {}", e);
                    }
                }
            }
        });
    }
}

impl Drop for PlayerPersistence {
    /// Save data when the persistence manager is dropped
    fn drop(&mut self) {
        if let Err(e) = self.save() {
            println!("[PERSISTENCE] Failed to save player data on shutdown: {}", e);
        }
    }
}