Project: IronVein - A Scalable Multiplayer Online World
1. Project Vision
IronVein is a ground-up rewrite of a TypeScript-based multiplayer world, re-engineered in Rust to achieve massive scalability and performance. The primary goal is to build a robust core architecture capable of supporting 1000+ concurrent players in a single world instance.
The project will consist of a simple online world where players appear as colored balls on a grid. They can see each other's real-time movement and click to navigate. This core functionality will serve as the foundation for a future MMORPG.
2. Core Architecture
The system is designed as a set of three distinct services deployed on Railway, following a modern microservices pattern.

```mermaid
graph TD
    subgraph "Player's Browser"
        A["Web Client (Bevy WASM)"]
    end

    subgraph "Railway Platform"
        B["Game Server (Rust/Axum/Bevy)"]
        C["PostgreSQL Database"]
        D["Static File Service for Client"]
    end

    A --"1. Load Client (HTTPS)"--> D
    A --"2. Connect (Secure WebSocket - WSS)"--> B
    B --"3. Persist/Load Data (TCP)"--> C
```

Game Server (The Core Logic): A headless Rust application that runs the main game loop, processes player input, handles all game logic (movement, state changes), and serves as the single source of truth for the game state.
Web Client (The View): A Rust application compiled to WebAssembly (WASM) that runs entirely in the user's browser. It is responsible for rendering the game world, capturing user input, and communicating with the Game Server.
Database (The Long-Term Memory): A managed PostgreSQL instance that persists all permanent data, such as player accounts, character positions, and inventory.
3. Tech Stack

| Component       | Technology      | Why?                                                                                              |
|-----------------|-----------------|---------------------------------------------------------------------------------------------------|
| Language        | Rust            | Provides memory safety, zero-cost abstractions, and world-class performance for high concurrency. |
| Game Engine     | Bevy            | A data-driven game engine using an Entity Component System (ECS) for maximum performance and scalability. Crucially, allows code sharing between server and client. |
| Web Server      | Axum            | A modern, ergonomic web framework for Rust, with first-class support for WebSockets.               |
| Database ORM    | SQLx            | An async, type-safe SQL toolkit that validates queries at compile time, preventing runtime errors.  |
| Database        | PostgreSQL      | A powerful, reliable, and battle-tested open-source relational database.                           |
| Serialization   | Serde + bincode | Serde provides the framework, and bincode provides an extremely fast and compact binary serialization format for network messages. |
| WASM Client     | Rust -> WASM    | Compiles our Rust client code to run at near-native speed directly in the browser.                 |
| Client Bundler  | Vite            | A fast and simple frontend tool to bundle the HTML, JS glue, and WASM for the browser.             |

4. Project Structure (Cargo Workspace)
The project will be structured as a Cargo workspace to facilitate code sharing.

```bash
ironvein/
├── Cargo.toml          # The workspace manifest, defining the members below.
│
├── .github/workflows/  # (Optional) For CI/CD on Railway.
│
├── client/
│   ├── Cargo.toml      # Bevy client crate dependencies.
│   ├── index.html      # The entry point for Vite.
│   ├── package.json    # Vite and other JS dependencies.
│   └── src/            # Rust source for the WASM client (rendering, input).
│
├── server/
│   ├── Cargo.toml      # Axum + Bevy headless server crate dependencies.
│   ├── Dockerfile      # For deploying the server to Railway.
│   └── src/            # Rust source for the game server (logic, networking).
│
└── shared/
    ├── Cargo.toml      # Shared crate dependencies (serde, bincode, bevy).
    └── src/            # Rust source for shared types (components, network messages).
```

5. Shared Data Structures (shared crate)
These are the core data types shared between the server and client. They must derive traits for Bevy's ECS and for network serialization.
File: shared/src/lib.rs

```rust
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
pub struct TargetDestination {
    pub x: f32,
    pub y: f32,
}

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
```

6. Communication Protocol (WebSocket)
All real-time communication happens over a single WebSocket connection. Messages are serialized with bincode.
File: shared/src/lib.rs (continued)

```rust
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
```

7. Database Schema (PostgreSQL)
A simple schema to store player data. The server will use SQLx to interact with this table.

```sql
-- Migration file: 001_create_players_table.sql
CREATE TABLE players (
    id BIGSERIAL PRIMARY KEY,
    username VARCHAR(32) UNIQUE NOT NULL,
    -- In a real app, this would be a securely hashed password.
    -- For this prototype, a simple text field is acceptable.
    password_hash TEXT NOT NULL,
    last_position_x REAL NOT NULL DEFAULT 0.0,
    last_position_y REAL NOT NULL DEFAULT 0.0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_login_at TIMESTAMPTZ
);
```

8. Development and Build Plan (Roadmap)
This will be implemented in sequential order.
- [x] Workspace Setup: Create the Cargo workspace and the server, client, and shared crates as defined in section 4.
- [x] Basic WebSocket Server: Implement a minimal Axum server that accepts a WebSocket connection, prints a message, and closes.
- [x] Basic WASM Client: Create a minimal Bevy client that compiles to WASM, draws a blue background, and connects to the WebSocket server.
- [x] Implement Protocol: Add the ClientToServerMsg and ServerToClientMsg enums to the shared crate. Implement bincode serialization/deserialization on both ends.
- [x] Player Spawning (Server):
  On WebSocket connect, spawn a new Bevy entity with Player, PlayerId, and a default Position.
  Broadcast a ServerToClientMsg::PlayerJoined message to all other clients.
  Send the ServerToClientMsg::Welcome message to the new client.
- [x] Player Rendering (Client):
  When the client receives Welcome, it should store its own PlayerId.
  When receiving a WorldStateSnapshot or PlayerJoined message, it should spawn entities with Position components.
  Render its own player entity as a blue ball and all other players as red balls.
- [x] Movement Logic:
  Client: On click, send a ClientToServerMsg::ClickPosition message.
  Server: On receiving ClickPosition, update the TargetDestination component for that player's entity.
  Server: Create a Bevy system that iterates through entities with Position and TargetDestination and moves them slightly closer each frame (simple linear interpolation).
  Server: Create a Bevy system that periodically broadcasts a WorldStateSnapshot of all player positions to all clients.
  Client: When receiving WorldStateSnapshot, update the Position of all rendered entities, which will move them on screen.
- [ ] Database Integration:
  Use sqlx-cli to set up migrations.
  On WebSocket connect (before spawning), perform a mock "login" and load/save the player's last_position from the PostgreSQL database.
- [ ] Deployment: Configure Dockerfile and railway.json to deploy the three services to Railway.
