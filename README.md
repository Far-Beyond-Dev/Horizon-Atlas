# Horizon Atlas

Horizon Atlas is a connection orchestrator for Horizon Game Server instances. It manages the lifecycle of game servers, including starting, stopping, and monitoring them as players move throughout the game world.

Designed to function as a WebSocket proxy and connection manager, Horizon Atlas dynamically prepares and launches game servers for specific regions as players approach. It also preemptively serializes player state and begins syncing it via delta compression to the new game server, ensuring seamless transitions and optimized performance.
