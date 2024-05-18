use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::accept_async;
use futures_util::{StreamExt, SinkExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use serde::{Serialize, Deserialize};
use log::{info, error};
use config::{Config, ConfigError, File, Environment};

#[derive(Debug, Serialize, Deserialize)]
struct InstanceInfo {
    id: String,
    address: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GameState {
    players: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ServerConfig {
    host: String,
    port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
struct DatabaseConfig {
    #[serde(rename = "type")]
    db_type: String,
    connection_string: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LoggingConfig {
    level: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct InstancesConfig {
    instance_1: String,
    instance_2: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AppConfig {
    server: ServerConfig,
    database: DatabaseConfig,
    logging: LoggingConfig,
    instances: InstancesConfig,
}

impl AppConfig {
    pub fn from_file(file: &str) -> Result<Self, ConfigError> {
        let settings = Config::builder()
            .add_source(File::with_name(file))
            .add_source(Environment::with_prefix("APP"))
            .build()?;
        
        settings.try_deserialize()
    }
}

type SharedGameState = Arc<Mutex<GameState>>;
type SharedInstanceInfo = Arc<Mutex<HashMap<String, InstanceInfo>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load configuration
    println!("Loading configs...");
    let config = AppConfig::from_file("config.toml")?;
    
    // Initialize the logger with the specified log level
    std::env::set_var("RUST_LOG", &config.logging.level);
    env_logger::init();

    println!("Initilizing Game server...");
    let game_state: SharedGameState = Arc::new(Mutex::new(GameState {
        players: HashMap::new(),
    }));

    let instance_info: SharedInstanceInfo = Arc::new(Mutex::new(HashMap::new()));

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&addr).await.expect("Can't bind to address");

    println!("Horizon Atlas server is running on {}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        let game_state = game_state.clone();
        let instance_info = instance_info.clone();

        tokio::spawn(async move {
            let ws_stream = accept_async(stream).await.expect("Error during the websocket handshake");
            let (mut ws_sender, mut ws_receiver) = ws_stream.split();

            while let Some(message) = ws_receiver.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        if let Ok(instance) = serde_json::from_str::<InstanceInfo>(&text) {
                            info!("Registering instance: {:?}", instance);
                            instance_info.lock().unwrap().insert(instance.id.clone(), instance);
                            ws_sender.send(Message::Text("Instance registered".to_string())).await.unwrap();
                        } else if let Ok(new_state) = serde_json::from_str::<GameState>(&text) {
                            info!("Received new game state: {:?}", new_state);
                            let mut state = game_state.lock().unwrap();
                            *state = new_state;
                        } else {
                            error!("Received invalid message: {}", text);
                        }
                    },
                    Ok(Message::Binary(_)) => error!("Unexpected binary message"),
                    Ok(Message::Close(_)) => {
                        info!("Connection closed");
                        break;
                    },
                    Err(e) => {
                        error!("Error receiving message: {}", e);
                        break;
                    },
                    _ => {}
                }
            }
        });
    }

    Ok(())
}
