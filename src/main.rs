use serde_json::{json, Value}; // Import json macro and Value from serde_json
use socketioxide::{
    extract::{AckSender, Bin, Data, SocketRef},
    SocketIo,
};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, event, info, Level};
use tracing_subscriber::FmtSubscriber;
use std::io::Write; // Bring the Write trait into scope
use viz::{handler::ServiceHandler, serve, Result, Router};
use serde::Deserialize;
use std::fs;

// Define a struct to represent the configuration
#[derive(Debug, Deserialize)]
struct Config {
    server: ServerConfig,
}

#[derive(Debug, Deserialize)]
struct ServerConfig {
    address: String,
    port: u16,
}

// Function to load the configuration from file
fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    let config_str = fs::read_to_string("config.toml")?;
    let config: Config = toml::from_str(&config_str)?;
    Ok(config)
}
// Define a struct for Player
#[derive(Debug)]
struct Player {
    id: String,
    socket: SocketRef, // Include the SocketRef to associate player with their connection
}

fn on_connect(socket: SocketRef, Data(data): Data<Value>, players: Arc<Mutex<Vec<Player>>>) {
    // Create a new Player instance and add it to the players array
    let player = Player {
        id: socket.id.to_string(), // Convert Sid to String
        socket: socket.clone(),    // Store the SocketRef
    };
    players.lock().unwrap().push(player);

    info!("Socket.IO connected: {:?} {:?}", socket.ns(), socket.id);
    socket.emit("auth", data).ok();

    let players_arc = Arc::clone(&players);

    socket.on(
        "UpdatePlayerLocation",
        |socket: SocketRef, Data::<Value>(data), Bin(bin)| {
            info!("Received event: {:?} {:?}", data, bin);
            socket.bin(bin).emit("message-back", data).ok();
        },
    );

    socket.on(
        "message-with-ack",
        |Data::<Value>(data), ack: AckSender, Bin(bin)| {
            info!("Received event: {:?} {:?}", data, bin);
            ack.bin(bin).send(data).ok();
        },
    );

    // Register the event handler to send online players array to the client
    socket.on(
        "getOnlinePlayers",
        move |socket: SocketRef, _: Data<Value>, _: Bin| {
            info!("Responding with online players list");
            let players = players.lock().unwrap(); // Lock mutex to access players array

            let online_players_json = serde_json::to_value(
                players 
                    .iter()
                    .map(|player| json!({ "id": &player.id })) // Use json! macro to create JSON object
                    .collect::<Vec<_>>(),
            )
            .unwrap(); // Serialize online players array to JSON

            debug!("Player Array as JSON{}", online_players_json);

            socket.emit("onlinePlayers", online_players_json).ok(); // Emit online players array to client
        },
    );

    // Broadcast a message to all connected clients
    socket.on("broadcastMessage", move |Data::<Value>(data), _: Bin| {
        let players = players_arc.lock().unwrap(); // Lock mutex to access players array
        for player in &*players {
            player.socket.emit("broadcastMessage", data.clone()).ok(); // Access the associated SocketRef and emit the message
        }
    });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Basic print statements to ensure they work before any other code
    std::io::stdout().flush().unwrap(); // Ensure the buffer is flushed

    // Load configuration
    let config = load_config()?;
    let address = format!("{}:{}", config.server.address, config.server.port);

    // Initialize the tracing subscriber with INFO log level
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    println!("Starting Horizon Atlas...");
    println!("");

    println!("+--------------------------------------------------------------------------------------------------------------------------------+");
    println!("|    __    __                         __                                          ______      __       __                        |");
    println!("|   |  |  |  |                       |  |                                        /      |    |  |     |  |                       |");
    println!("|   | $$  | $$   ______     ______    |$$  ________    ______    _______        |  $$$$$$|  _| $$_    | $$   ______     _______  |");
    println!("|   | $$__| $$  /      |   /      |  |  | |        |  /      |  |       |       | $$__| $$ |   $$ |   | $$  |      |   /       | |");
    println!("|   | $$    $$ |  $$$$$$| |  $$$$$$| | $$  |$$$$$$$$ |  $$$$$$| | $$$$$$$|      | $$    $$  |$$$$$$   | $$   |$$$$$$| |  $$$$$$$ |");
    println!("|   | $$$$$$$$ | $$  | $$ | $$   |$$ | $$   /    $$  | $$  | $$ | $$  | $$      | $$$$$$$$   | $$ __  | $$  /      $$  |$$    |  |");
    println!("|   | $$  | $$ | $$__/ $$ | $$       | $$  /  $$$$_  | $$__/ $$ | $$  | $$      | $$  | $$   | $$|  | | $$ |  $$$$$$$  _|$$$$$$| |");
    println!("|   | $$  | $$  |$$    $$ | $$       | $$ |  $$    |  |$$    $$ | $$  | $$      | $$  | $$    |$$  $$ | $$  |$$    $$ |       $$ |");
    println!("|    |$$   |$$   |$$$$$$   |$$        |$$  |$$$$$$$$   |$$$$$$   |$$   |$$       |$$   |$$     |$$$$   |$$   |$$$$$$$  |$$$$$$$  |");
    println!("|                                                                                                                                |");
    println!("+--------------------------------------------------------------------------------------------------------------------------------+");
                                                                                                                
                                                                                                                      
    println!("Horizon Atlas is running on {}", address);
    // Initialize the players array as an Arc<Mutex<Vec<Player>>>
    let players: Arc<Mutex<Vec<Player>>> = Arc::new(Mutex::new(Vec::new()));

    let (svc, io) = socketioxide::SocketIo::new_svc();

    let players_clone = players.clone();
    io.ns("/", move |socket, data| {
        on_connect(socket, data, players_clone.clone())
    });

    let app = Router::new()
        .get("/", |_| async { Ok("Hello, World!") })
        .any("/*", ServiceHandler::new(svc));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();

    info!("Server listening on interface 0.0.0.0 with port 3000");
    std::io::stdout().flush().unwrap(); // Ensure the buffer is flushed

    if let Err(e) = serve(listener, app).await {
        error!("{}", e);
    }

    Ok(())
}