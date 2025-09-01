use std::fmt;
use std::io;
use std::net::AddrParseError;

/// Comprehensive error handling for the Horizon Atlas proxy
#[derive(Debug)]
pub enum ProxyError {
    /// Network I/O errors
    Io(io::Error),
    /// Address parsing errors
    AddrParse(AddrParseError),
    /// Connection errors
    Connection(String),
    /// Server transfer errors
    Transfer(String),
    /// Configuration errors
    Config(String),
    /// Client handling errors
    Client(String),
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxyError::Io(err) => write!(f, "I/O error: {}", err),
            ProxyError::AddrParse(err) => write!(f, "Address parse error: {}", err),
            ProxyError::Connection(msg) => write!(f, "Connection error: {}", msg),
            ProxyError::Transfer(msg) => write!(f, "Transfer error: {}", msg),
            ProxyError::Config(msg) => write!(f, "Configuration error: {}", msg),
            ProxyError::Client(msg) => write!(f, "Client error: {}", msg),
        }
    }
}

impl std::error::Error for ProxyError {}

impl From<io::Error> for ProxyError {
    fn from(err: io::Error) -> Self {
        ProxyError::Io(err)
    }
}

impl From<AddrParseError> for ProxyError {
    fn from(err: AddrParseError) -> Self {
        ProxyError::AddrParse(err)
    }
}

pub type Result<T> = std::result::Result<T, ProxyError>;