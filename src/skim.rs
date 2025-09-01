use std::sync::mpsc;
use std::thread;
use crate::error::Result;

/// Data skimming functionality for analyzing traffic without blocking passthrough
pub struct DataSkimmer {
    tx: mpsc::Sender<SkimData>,
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
    pub fn new() -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        
        thread::spawn(move || {
            Self::skim_worker(rx);
        });
        
        Ok(Self { tx })
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
    fn skim_worker(rx: mpsc::Receiver<SkimData>) {
        for skim_data in rx {
            Self::process_data(&skim_data);
        }
    }
    
    /// Process individual data packets
    fn process_data(skim_data: &SkimData) {
        if let Ok(text) = std::str::from_utf8(&skim_data.data) {
            // Look for client movement commands
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

impl Default for DataSkimmer {
    fn default() -> Self {
        Self::new().expect("Failed to create data skimmer")
    }
}