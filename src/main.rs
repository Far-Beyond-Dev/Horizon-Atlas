use serde::{Deserialize, Serialize};
use socketioxide::{SocketIo, SocketIoConfig, SocketRef, extract::{Data, Bin}};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{info, error};
use tracing_subscriber::FmtSubscriber;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerInfo {
    id: String,
    address: String,
    load: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlayerAssignment {
    player_id: String,
    server_id: String,
}

struct MasterState {
    servers: Arc<Mutex<HashMap<String, ServerInfo>>>,
    player_assignments: Arc<Mutex<HashMap<String, String>>>,
}

impl MasterState {
    async fn assign_player(&self, player_id: String) -> Result<String, Box<dyn std::error::Error>> {
        let servers = self.servers.lock().unwrap();
        if let Some((server_id, _)) = servers.iter().min_by_key(|&(_, info)| info.load) {
            let server_id = server_id.clone();
            let mut assignments = self.player_assignments.lock().unwrap();
            assignments.insert(player_id.clone(), server_id.clone());
            Ok(server_id)
        } else {
            Err("No available servers".into())
        }
    }

    async fn handle_server_registration(&self, server_info: ServerInfo) {
        let mut servers = self.servers.lock().unwrap();
        servers.insert(server_info.id.clone(), server_info);
    }
}

async fn on_connect(socket: SocketRef, state: Arc<MasterState>) {
    let state_clone = Arc::clone(&state);

    socket.on("register_server", move |Data::<ServerInfo>(server_info), _: Bin| {
        let state_clone = Arc::clone(&state_clone);
        async move {
            info!("Registering server: {:?}", server_info);
            state_clone.handle_server_registration(server_info).await;
            socket.emit("server_registered", true).ok();
        }
    });

    let state_clone = Arc::clone(&state);
    socket.on("assign_player", move |Data::<String>(player_id), ack| {
        let state_clone = Arc::clone(&state_clone);
        async move {
            match state_clone.assign_player(player_id.clone()).await {
                Ok(server_id) => {
                    ack.send(server_id).ok();
                }
                Err(e) => {
                    error!("Failed to assign player: {}", e);
                    ack.send("error".to_string()).ok();
                }
            }
        }
    });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let state = Arc::new(MasterState {
        servers: Arc::new(Mutex::new(HashMap::new())),
        player_assignments: Arc::new(Mutex::new(HashMap::new())),
    });

    let socketio_config = SocketIoConfig::default();

    let io = SocketIo::new(socketio_config);
    let state_clone = Arc::clone(&state);

    io.ns("/", move |socket| {
        on_connect(socket, Arc::clone(&state_clone))
    });

    let addr = "0.0.0.0:8080".parse().unwrap();
    info!("Master server running on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    socketioxide::serve(listener, io).await;

    Ok(())
}
