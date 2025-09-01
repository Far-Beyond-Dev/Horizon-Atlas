use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use crate::error::{ProxyError, Result};

static CLIENT_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Client connection information and state
#[derive(Debug, Clone)]
pub struct ClientInfo {
    /// Unique client identifier
    pub id: String,
    /// Client socket address
    pub addr: SocketAddr,
    /// Currently connected server ID
    pub current_server: String,
    /// Connection start time
    pub connected_at: SystemTime,
    /// Number of bytes transferred
    pub bytes_transferred: u64,
    /// Client state
    pub state: ClientState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientState {
    /// Initial connection state
    Connecting,
    /// Successfully connected and proxying
    Connected,
    /// In process of being transferred to another server
    Transferring {
        from_server: String,
        to_server: String,
    },
    /// Connection closed
    Disconnected,
    /// Error state
    Error(String),
}

impl ClientInfo {
    /// Create a new client info
    pub fn new(addr: SocketAddr, server_id: String) -> Self {
        let id = Self::generate_client_id();
        
        Self {
            id,
            addr,
            current_server: server_id,
            connected_at: SystemTime::now(),
            bytes_transferred: 0,
            state: ClientState::Connecting,
        }
    }
    
    /// Generate a unique client ID
    fn generate_client_id() -> String {
        let counter = CLIENT_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("client-{}", counter)
    }
    
    /// Update client state
    pub fn set_state(&mut self, state: ClientState) {
        self.state = state;
    }
    
    /// Add transferred bytes
    pub fn add_bytes_transferred(&mut self, bytes: u64) {
        self.bytes_transferred += bytes;
    }
    
    /// Get connection duration
    pub fn connection_duration(&self) -> std::time::Duration {
        self.connected_at.elapsed().unwrap_or_default()
    }
    
    /// Check if client is in a state that allows transfer
    pub fn can_transfer(&self) -> bool {
        matches!(self.state, ClientState::Connected)
    }
}

/// Client connection wrapper with additional functionality
pub struct ClientConnection {
    /// Client stream
    pub stream: TcpStream,
    /// Client information
    pub info: Arc<std::sync::Mutex<ClientInfo>>,
}

impl ClientConnection {
    /// Create a new client connection
    pub fn new(stream: TcpStream, server_id: String) -> Result<Self> {
        let addr = stream.peer_addr()?;
        let info = Arc::new(std::sync::Mutex::new(ClientInfo::new(addr, server_id)));
        
        Ok(Self { stream, info })
    }
    
    /// Clone the stream for bidirectional communication
    pub fn try_clone(&self) -> Result<TcpStream> {
        self.stream.try_clone().map_err(ProxyError::from)
    }
    
    /// Get client ID
    pub fn client_id(&self) -> String {
        self.info.lock().unwrap().id.clone()
    }
    
    /// Update client state
    pub fn set_state(&self, state: ClientState) {
        if let Ok(mut info) = self.info.lock() {
            info.set_state(state);
        }
    }
    
    /// Add transferred bytes
    pub fn add_bytes_transferred(&self, bytes: u64) {
        if let Ok(mut info) = self.info.lock() {
            info.add_bytes_transferred(bytes);
        }
    }
    
    /// Get current server ID
    pub fn current_server(&self) -> String {
        self.info.lock().unwrap().current_server.clone()
    }
    
    /// Check if client can be transferred
    pub fn can_transfer(&self) -> bool {
        self.info.lock().unwrap().can_transfer()
    }
}