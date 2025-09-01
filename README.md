![logo-no-background](branding/logo-no-background.png)

# Horizon Atlas

**⚠️ WORK IN PROGRESS ⚠️** [Website](https://horizon.farbeyond.dev/atlas)

A high-performance, production-ready WebSocket proxy and load balancer designed for distributed game server architectures. Atlas intelligently routes client connections across multiple game servers while providing seamless server transitions, real-time traffic analysis, and advanced clustering capabilities.

## 🚀 Features

### Core Functionality
- **Multi-Server Load Balancing** - Automatically distribute clients across multiple backend game servers
- **Seamless Client Transfers** - Move players between servers without connection drops
- **WebSocket Proxy** - High-performance TCP passthrough with WebSocket support
- **Real-time Traffic Analysis** - Non-blocking data skimming for monitoring and debugging
- **Health Monitoring** - Automatic server health checks with failover capabilities

### Advanced Capabilities
- **Predictive Server Transitions** - Detect client movement patterns to preload server instances
- **Cross-Server Communication** - Facilitate object and player transitions between server boundaries  
- **Cryptographic Operations** - Handle security operations without overloading game servers
- **Production-Ready Architecture** - Comprehensive error handling, logging, and monitoring

## 🏗️ Architecture

Atlas operates as a clustered service sitting in front of your game server infrastructure:

```
Clients → Atlas Proxy → Game Server Cluster
                    ├── game-server-1 (capacity: 1000)
                    ├── game-server-2 (capacity: 1000) 
                    └── game-server-3 (capacity: 1000)
```

### Modular Design
- **`proxy`** - Main proxy server and connection handling
- **`server`** - Backend server management and load balancing
- **`client`** - Client connection tracking and state management
- **`transfer`** - Server-to-server client migration system
- **`skim`** - Real-time traffic analysis and monitoring
- **`config`** - Configuration management and server definitions
- **`error`** - Comprehensive error handling and result types

## 🚦 Getting Started

### Prerequisites
- Rust 1.70+ 
- Multiple backend game servers running on different ports

### Installation
```bash
git clone https://github.com/your-org/horizon-atlas
cd horizon-atlas
cargo build --release
```

### Configuration
Atlas supports multiple backend servers out of the box. Configure your server endpoints in `src/main.rs`:

```rust
let config = ProxyConfig::new(
    "0.0.0.0:9000",  // Atlas listen address
    vec![
        ("127.0.0.1:8080", "game-server-1"),
        ("127.0.0.1:8081", "game-server-2"), 
        ("127.0.0.1:8082", "game-server-3"),
    ],
)?;
```

### Running
```bash
cargo run
```

Atlas will start listening on `0.0.0.0:9000` and automatically load balance connections across your configured game servers.

## 🎯 Use Cases

### Game Server Clustering
- **MMO Server Architecture** - Handle thousands of concurrent players across multiple server instances
- **Battle Royale Games** - Dynamically balance players as matches start and end
- **Open World Games** - Seamlessly transition players between server regions

### Development & Testing
- **Load Testing** - Distribute test clients across multiple server instances
- **A/B Testing** - Route different client groups to different server versions
- **Staging Environments** - Mirror production traffic across test servers

## 📊 Monitoring & Analytics

Atlas provides built-in monitoring capabilities:

### Real-time Metrics
- Active connection counts per server
- Server health status and load percentages
- Data transfer rates and client session durations
- Client movement pattern detection

### Traffic Analysis
```rust
// Example: Detecting client movement commands
[1234567890] Client client-1 movement detected: {"client":"move","x":123,"y":456}
[LOAD_BALANCER] Managing 150 active connections
[TRANSFER] Notifying transfer: client-5 from game-server-1 to game-server-2
```

## 🔄 Client Transfer System

One of Atlas's key features is seamless client server transitions:

1. **Movement Detection** - Monitor client data for boundary-crossing indicators
2. **Advance Notification** - Inform both servers about incoming transfer
3. **Connection Migration** - Move client to new server without dropping connection
4. **State Synchronization** - Ensure game state consistency across transition

### Transfer Reasons
- **Load Balancing** - Move clients from overloaded servers
- **Server Maintenance** - Migrate clients before server updates
- **Geographic Optimization** - Route clients to nearest server
- **Administrative Actions** - Manual client relocations

## 🛠️ Development

### Project Structure
```
src/
├── main.rs          # Application entry point
├── lib.rs           # Module declarations
├── proxy.rs         # Main proxy server logic
├── server.rs        # Backend server management  
├── client.rs        # Client connection handling
├── transfer.rs      # Client migration system
├── skim.rs          # Traffic analysis
├── config.rs        # Configuration management
└── error.rs         # Error types and handling
```

### Key Components

#### ProxyConfig
Central configuration for server endpoints, load balancing algorithms, and connection limits.

#### ClientConnection  
Wrapper for TCP streams with state tracking, byte counting, and transfer capabilities.

#### ServerManager
Manages backend server pool with health monitoring and connection count tracking.

#### TransferManager
Handles client migrations between servers with advance notifications and state coordination.

#### DataSkimmer
Non-blocking traffic analysis for detecting movement patterns and game events.

## 🔧 Configuration Options

### Load Balancing Algorithms
- **Least Connections** - Route to server with fewest active connections (default)
- **Round Robin** - Cycle through servers sequentially  
- **Random** - Distribute connections randomly

### Server Settings
- **Buffer Size** - Data transfer buffer size (default: 4096 bytes)
- **Max Connections** - Maximum concurrent connections per server
- **Health Check Interval** - Server health monitoring frequency
- **Transfer Timeout** - Maximum time for client migrations

## 🚧 Roadmap

- [ ] **Configuration Files** - YAML/TOML config file support
- [ ] **Metrics API** - REST endpoint for monitoring integration
- [ ] **SSL/TLS Support** - Encrypted client connections
- [ ] **Authentication Layer** - Client authentication and authorization
- [ ] **Persistent Sessions** - Client reconnection with session recovery
- [ ] **Docker Support** - Container deployment configurations
- [ ] **Kubernetes Integration** - Cloud-native deployment manifests

## 🤝 Contributing

We welcome contributions! Please see our [Contributing Guide](CONTRIBUTING.md) for details.

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🆘 Support

- **Documentation** - [Website](https://horizon.farbeyond.dev/atlas)
- **Issues** - [GitHub Issues](https://github.com/your-org/horizon-atlas/issues)
- **Discussions** - [GitHub Discussions](https://github.com/your-org/horizon-atlas/discussions)

---

**Atlas** - Intelligent proxy infrastructure for distributed game servers.