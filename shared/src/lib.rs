use bevy::prelude::Component;
use serde::{Deserialize, Serialize};

// A unique identifier for a player entity.
#[derive(Component, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerId(pub u64);

// The player's current position on the grid.
#[derive(Component, Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

// The player's target destination. The server will move the player towards this.
#[derive(Component, Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct TargetDestination(pub Position);

// A simple component to identify the player entity.
#[derive(Component, Serialize, Deserialize, Debug, Clone)]
pub struct Player;

// A simple component to identify entities controlled by other players.
#[derive(Component, Serialize, Deserialize, Debug, Clone)]
pub struct Enemy;

// Game configuration that can be sent to the client on connection.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GameConfig {
    pub map_width: u32,
    pub map_height: u32,
}

// Messages sent FROM the Client TO the Server
#[derive(Serialize, Deserialize, Debug)]
pub enum ClientToServerMsg {
    // Sent when the player clicks on the map.
    ClickPosition { x: f32, y: f32 },
    // A keep-alive message.
    Ping,
}

// Messages sent FROM the Server TO the Client
#[derive(Serialize, Deserialize, Debug)]
pub enum ServerToClientMsg {
    // Sent once on connection to give the client its ID and the game config.
    Welcome { player_id: PlayerId, config: GameConfig },
    // A full snapshot of all player positions. Sent periodically.
    WorldStateSnapshot(Vec<(PlayerId, Position)>),
    // Informs clients that a new player has joined.
    PlayerJoined(PlayerId, Position),
    // Informs clients that a player has left.
    PlayerLeft(PlayerId),
    // A keep-alive response.
    Pong,
} 