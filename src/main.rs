//! Horizon Atlas
//! 
//! Horizon Atlas is a connection orchestrator for Horizon Game Server instances.
//! It manages the lifecycle of game servers, including starting, stopping, and monitoring them
//! as players move in the game world.
//! 
//! Horizon Atlas is designed to be used as a websoctet proxy and connection manager, As a player approaches a region,
//! the server will prepare the game server for that region if it is not already running, it will also as they get closer preemptively
//! serialize player state and begin syncing it via delta compression to the new game server they are approaching.

