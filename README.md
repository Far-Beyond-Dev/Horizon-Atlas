![logo-no-background](branding/logo-no-background.png)

# Horizon Atlas

Horizon Atlas is a connection orchestrator for Horizon Game Server instances. Its primary goal is to manage the lifecycle of game servers, including starting, stopping, and monitoring them as players move throughout the game world.

## Goals

- **Dynamic Server Management:** Automatically start, stop, and monitor game server instances based on player movement and activity.
- **Seamless Player Experience:** As players approach new regions, Horizon Atlas prepares the corresponding game server if it is not already running, ensuring smooth transitions.
- **Efficient State Synchronization:** Preemptively serialize player state and begin syncing it via delta compression to the new game server as players move closer to a region.
- **WebSocket Proxy:** Act as a connection manager and proxy, routing player connections to the appropriate game server instance.
- **Scalability:** Enable large, persistent worlds by efficiently orchestrating multiple game server instances.

## Project Status

This project is in the early planning phase. There is currently no code or dependencies. The README will be updated as development progresses.

## License

To be determined.
