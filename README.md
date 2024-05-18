![logo-no-background](branding\logo-no-background.png)

# Horizon Atlas

Welcome to Horizon Atlas, the master server software for the Horizon game server architecture. Horizon Atlas serves as the central hub linking multiple Horizon instances together, ensuring a seamless and interconnected gaming experience across geographically distributed servers.

## Table of contents

- [Horizon Atlas](#horizon-atlas)
  * [Overview](#overview)
  * [Features](#features)
  * [Installation](#installation)
  * [Configuration](#configuration)
  * [Usage](#usage)
    + [API Endpoints](#api-endpoints)
    + [Example API Request](#example-api-request)
  * [Contributing](#contributing)

## Overview
Horizon Atlas is designed to maintain the entire game state, while individual Horizon instances manage local game states for directly connected players. This architecture allows for seamless travel between in-game regions with no loading screens, providing players with a continuous and immersive experience.

## Features
  - Centralized Game State Management: Horizon Atlas holds the complete game state, synchronizing with local game states on individual instances to ensure consistency and coherence.

  - Seamless Regional Transitions: Enables players to move between different in-game regions without interruption, leveraging geo-pinned instances.

  - Scalable Architecture: Easily scales with the addition of new Horizon instances to accommodate growing player bases and expanding game worlds.

  - High Availability: Designed for reliability and redundancy to ensure continuous operation and minimal downtime.

## Installation

To install Horizon Atlas, follow these steps:

1.  Clone the Repository:

    ```bash
    git clone https://github.com/yourusername/horizon-atlas.git
    cd horizon-atlas
    ```

2.  Build the Project:

    ```bash
    cargo build --release
    ```

3.  Run the Server:

    ```bash
    ./target/release/horizon-atlas
    ```

## Configuration

Horizon Atlas requires a configuration file to set up its parameters. Below is a sample configuration file (`config.toml`):


```toml
[server]
host = "0.0.0.0"
port = 3000

[logging]
level = "info"

[instances]
instance_1 = "192.168.1.2:9000"
instance_2 = "192.168.1.3:9000"
```

-   server: The host and port where Horizon Atlas will run.
-   database: The database type and connection string for storing the game state.
-   logging: The logging level (e.g., debug, info, warn, error).
-   instances: A list of Horizon instances with their respective IP addresses and ports.

Usage
-----

Once Horizon Atlas is running, it will automatically start managing connections between the Horizon instances and synchronize their local game states with the central game state.

### API Endpoints

Horizon Atlas provides a RESTful API for managing the game state and instances. Some key endpoints include:

-   `GET /status`: Returns the current status of Horizon Atlas and connected instances.
-   `POST /instances`: Add a new Horizon instance.
-   `DELETE /instances/:id`: Remove an existing Horizon instance.
-   `GET /gamestate`: Retrieve the current game state.
-   `POST /gamestate`: Update the game state.

### Example API Request

To add a new instance:

```bash
curl -X POST http://localhost:8080/instances -d '{"address": "192.168.1.4:9000"}'
```

## Contributing

We welcome contributions to Horizon Atlas! Please fork the repository and submit pull requests for any features, improvements, or bug fixes.

1.  Fork the repository
2.  Create a new branch (`git checkout -b feature-branch`)
3.  Commit your changes (`git commit -am 'Add new feature'`)
4.  Push to the branch (`git push origin feature-branch`)
5.  Create a new Pull Request