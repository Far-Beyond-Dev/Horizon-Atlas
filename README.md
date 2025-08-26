![logo-no-background](branding/logo-no-background.png)

# Horizon Atlas

**⚠️ WORK IN PROGRESS ⚠️** [Website](https://horizon.farbeyond.dev/atlas)

Atlas is a clustered service that sits in front of game servers, providing WebSocket proxying, load balancing, and seamless cross-server transitions for distributed gaming environments.

## Features

- **WebSocket Proxying**: Handles client-server communication with intelligent routing
- **Load Balancing**: Multiple algorithms including round-robin, least connections, and geographic routing
- **Cross-Server Transitions**: Seamless player movement across server boundaries
- **Cryptographic Security**: Built-in encryption and key management without server overhead
- **Health Monitoring**: Automatic failover and health checking
- **Region Management**: Position-based server selection with auto-scaling
- **Metrics & Monitoring**: Comprehensive performance tracking
- **Clustering**: Distributed consensus using Raft algorithm

## Prerequisites

- Rust 1.70 or later
- Cargo

## Installation

### From Source

```bash
git clone https://github.com/your-org/Horizon-Atlas.git
cd Horizon-Atlas
cargo build --release
```

## Configuration

Copy the example configuration and customize it for your environment:

```bash
cp atlas.example.toml atlas.toml
```

Key configuration sections:

- **Server**: Basic server settings (bind address, connections, timeouts)
- **Cluster**: Node identification and consensus configuration
- **Discovery**: Service discovery and backend server configuration
- **Routing**: Load balancing and failover settings
- **Regions**: Geographic boundaries and auto-scaling rules

See `atlas.example.toml` for detailed configuration options.

## Usage

### Development

```bash
# Run with default configuration
cargo run

# Run with custom config
cargo run -- --config custom-atlas.toml
```

### Production

```bash
# Build optimized binary
cargo build --release

# Run the binary
./target/release/Horizon-Atlas --config atlas.toml
```

## Architecture

Atlas operates as a distributed proxy layer:

1. **Client Connection**: Clients connect to Atlas nodes via WebSocket
2. **Server Discovery**: Atlas discovers and monitors backend game servers
3. **Intelligent Routing**: Routes messages based on player position and server health
4. **Cross-Server Handling**: Manages transitions when players cross region boundaries
5. **Load Balancing**: Distributes load across available servers using configurable algorithms

## Development

### Building

```bash
cargo build
```

### Testing

```bash
cargo test
```

### Running with Debug Logging

```bash
RUST_LOG=debug cargo run
```

## Contributing

This project is in active development. Please see the GitHub issues for current priorities.

## License

[Add your license information here]