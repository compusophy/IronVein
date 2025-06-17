use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use bevy::{prelude::*, tasks::{IoTaskPool, Task}, time::FixedTime};
use futures::{sink::SinkExt, StreamExt, future::FutureExt};
use shared::*;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use bincode;
use dotenvy::dotenv;
use sqlx::{FromRow, PgPool};
use std::env;
use tracing::{info, warn};
use std::sync::Arc;

// --- Core App State and Commands ---

/// A command sent from the web server thread to the Bevy world thread.
enum WorldCommand {
    Connect {
        client_tx: mpsc::Sender<Message>,
    },
    Disconnect {
        player_id: PlayerId,
    },
}

/// A message sent from a client's websocket to the Bevy world.
struct ClientMessage {
    player_id: PlayerId,
    message: ClientToServerMsg,
}

/// Axum's state, shared with all websocket handlers.
#[derive(Clone)]
struct AppState {
    world_cmd_tx: mpsc::Sender<WorldCommand>,
    client_msg_tx: mpsc::Sender<ClientMessage>,
    db_pool: PgPool,
}

// --- Bevy Resources for holding the channels ---

#[derive(Resource)]
struct WorldCommandRx(mpsc::Receiver<WorldCommand>);

#[derive(Resource)]
struct ClientMessageRx(mpsc::Receiver<ClientMessage>);

#[derive(Resource, Deref, DerefMut, Default)]
struct Clients(Vec<mpsc::Sender<Message>>);

#[derive(Event)]
struct PlayerDisconnectEvent {
    player_id: PlayerId,
}

// --- Main Application Setup ---

#[tokio::main]
async fn main() {
    dotenv().ok();
    tracing_subscriber::fmt::init();

    // --- Connect to Database ---
    let db_pool = PgPool::connect(&env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Failed to connect to database");
    info!("Database connection established.");

    // --- Create Communication Channels ---
    let (world_cmd_tx, world_cmd_rx) = mpsc::channel(100);
    let (client_msg_tx, client_msg_rx) = mpsc::channel(1000);

    // --- Start Bevy App in a separate thread ---
    let bevy_thread = std::thread::spawn(move || {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.insert_resource(FixedTime::new_from_secs(1.0 / 30.0));
        app.insert_resource(Clients::default());
        app.insert_resource(WorldCommandRx(world_cmd_rx));
        app.insert_resource(ClientMessageRx(client_msg_rx));
        app.insert_resource(db_pool.clone());
        app.add_event::<PlayerDisconnectEvent>();

        app.add_systems(Update, 
            (
                handle_world_commands, 
                handle_client_messages,
                save_player_position_on_disconnect
            ).chain()
        );

        app.add_systems(FixedUpdate, (move_players_system, broadcast_world_state_system));
        
        info!("Starting Bevy app...");
        app.run();
    });

    // --- Start Axum Web Server ---
    let axum_state = AppState {
        world_cmd_tx,
        client_msg_tx,
        db_pool,
    };

    let router = Router::new()
        .route("/game", get(ws_handler))
        .with_state(axum_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    info!("Axum server listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, router.into_make_service())
        .await
        .unwrap();

    bevy_thread.join().unwrap();
}

// --- WebSocket Handling ---

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    info!("New WebSocket connection");
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Create a channel for this client to receive messages from the world
    let (client_tx, mut client_rx) = mpsc::channel(100);

    // Tell the Bevy world about the new client
    if state.world_cmd_tx.send(WorldCommand::Connect { client_tx }).await.is_err() {
        warn!("World command channel closed, cannot connect client.");
        return;
    }

    // This task will forward messages from Bevy to the client's websocket
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // This task will forward messages from the client's websocket to the Bevy world
    let player_id_arc = Arc::new(tokio::sync::Mutex::new(None));
    let mut recv_task = tokio::spawn({
        let state = state.clone();
        let player_id_arc = player_id_arc.clone();
        async move {
            while let Some(Ok(msg)) = ws_receiver.next().await {
                if let Message::Binary(bytes) = msg {
                    // The first message from the client *should* be a login/auth message
                    // but for now we deserialize any ClientToServerMsg
                    if let Ok(client_msg) = bincode::deserialize::<ClientToServerMsg>(&bytes) {
                        if let Some(player_id) = *player_id_arc.lock().await {
                             let msg = ClientMessage { player_id, message: client_msg };
                             if state.client_msg_tx.send(msg).await.is_err() {
                                break;
                             }
                        }
                    }
                }
            }
        }
    });

    // Wait for either the send or recv task to complete
    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    // Client disconnected, tell the Bevy world
    if let Some(player_id) = *player_id_arc.lock().await {
         if state.world_cmd_tx.send(WorldCommand::Disconnect { player_id }).await.is_err() {
            warn!("World command channel closed, could not disconnect player {:?}", player_id);
        }
    }
    info!("WebSocket connection closed");
}

// --- Bevy Systems ---

/// Handles commands sent from the Axum server, like connect/disconnect.
fn handle_world_commands(
    mut commands: Commands,
    mut world_cmd_rx: ResMut<WorldCommandRx>,
    mut clients: ResMut<Clients>,
    db_pool: Res<PgPool>,
) {
    while let Ok(cmd) = world_cmd_rx.0.try_recv() {
        match cmd {
            WorldCommand::Connect { client_tx } => {
                info!("Connecting new client...");
                let client_id = clients.len();
                clients.push(client_tx.clone());
                
                let pool = db_pool.clone();
                let task = IoTaskPool::get().spawn(async move {
                    let username = format!("Player_{}", client_id);
                     let user_record = sqlx::query_as::<_, DbPlayer>("SELECT id, last_position_x, last_position_y FROM players WHERE username = $1")
                        .bind(&username)
                        .fetch_optional(&pool)
                        .await
                        .ok().flatten();

                    if let Some(player) = user_record {
                        info!("Found existing player: {}", player.id);
                        (PlayerId(player.id as u64), Position { x: player.last_position_x, y: player.last_position_y })
                    } else {
                         let new_player = sqlx::query_as::<_, DbPlayer>("INSERT INTO players (username, password_hash) VALUES ($1, $2) RETURNING id, last_position_x, last_position_y")
                            .bind(&username)
                            .bind("password")
                            .fetch_one(&pool)
                            .await
                            .unwrap();
                        info!("Created new player: {}", new_player.id);
                        (PlayerId(new_player.id as u64), Position { x: new_player.last_position_x, y: new_player.last_position_y })
                    }
                });

                commands.spawn(task.then(move |(player_id, start_pos)| {
                    // This command runs on the main Bevy thread once the async task is complete
                    move |world: &mut World| {
                        // Spawn the player entity
                        world.spawn((
                            Player,
                            player_id,
                            start_pos,
                            TargetDestination(start_pos),
                        ));

                        // Get the client's sender channel
                        let clients = world.resource::<Clients>();
                        if let Some(client_tx) = clients.get(client_id) {
                            // Send Welcome message
                            let welcome_msg = ServerToClientMsg::Welcome { player_id, config: GameConfig { map_width: 800, map_height: 600 } };
                            let welcome_msg_binary = bincode::serialize(&welcome_msg).unwrap();
                            client_tx.try_send(Message::Binary(welcome_msg_binary)).ok();

                             // Broadcast PlayerJoined message
                            let joined_msg = ServerToClientMsg::PlayerJoined(player_id, start_pos);
                            let joined_msg_binary = bincode::serialize(&joined_msg).unwrap();
                            let clients = clients.clone(); // Clone for the async task
                            IoTaskPool::get().spawn(async move {
                                broadcast_message(&clients, Message::Binary(joined_msg_binary), Some(client_id)).await;
                            }).detach();
                        }
                    }
                }));
            }
            WorldCommand::Disconnect { player_id } => {
                info!("Player {:?} disconnected", player_id);
                // Send an event to trigger saving their data and despawning
                let mut disconnect_events = commands.get_resource_mut::<Events<PlayerDisconnectEvent>>().unwrap();
                disconnect_events.send(PlayerDisconnectEvent { player_id });
            }
        }
    }
}

/// Handles incoming messages from all connected clients.
fn handle_client_messages(
    mut query: Query<(&PlayerId, &mut TargetDestination)>,
    mut client_msg_rx: ResMut<ClientMessageRx>,
) {
    while let Ok(ClientMessage { player_id, message }) = client_msg_rx.0.try_recv() {
        match message {
            ClientToServerMsg::ClickPosition { x, y } => {
                for (p_id, mut target_dest) in query.iter_mut() {
                    if *p_id == player_id {
                        target_dest.0.x = x;
                        target_dest.0.y = y;
                        break;
                    }
                }
            }
            ClientToServerMsg::Ping => {}
        }
    }
}

/// Moves players towards their target destination.
fn move_players_system(mut query: Query<(&mut Position, &TargetDestination)>) {
    for (mut pos, target) in query.iter_mut() {
        let dx = target.0.x - pos.x;
        let dy = target.0.y - pos.y;
        if dx.abs() > 0.01 || dy.abs() > 0.01 {
            pos.x += dx * 0.1;
            pos.y += dy * 0.1;
        }
    }
}

/// Periodically sends the state of the world to all clients.
fn broadcast_world_state_system(
    query: Query<(&PlayerId, &Position)>,
    clients: Res<Clients>,
) {
    let players: Vec<(PlayerId, Position)> = query.iter().map(|(pid, pos)| (*pid, *pos)).collect();
    if players.is_empty() { return; }

    let msg = ServerToClientMsg::WorldStateSnapshot(players);
    let binary_msg = bincode::serialize(&msg).unwrap();
    
    let clients = clients.clone();
    IoTaskPool::get().spawn(async move {
        broadcast_message(&clients, Message::Binary(binary_msg), None).await;
    }).detach();
}

/// Helper function to send a message to all clients.
async fn broadcast_message(
    clients: &Clients,
    msg: Message,
    exclude_id: Option<usize>,
) {
    for (i, client) in clients.iter().enumerate() {
        if exclude_id.map_or(true, |id| i != id) {
            if client.send(msg.clone()).await.is_err() {
                // Client is likely disconnected, the cleanup logic will handle it
            }
        }
    }
}

/// On disconnect, saves the player's last position and despawns their entity.
fn save_player_position_on_disconnect(
    mut events: EventReader<PlayerDisconnectEvent>,
    mut query: Query<(Entity, &PlayerId, &Position)>,
    db_pool: Res<PgPool>,
    mut commands: Commands,
) {
    for event in events.read() {
        for (entity, player_id, position) in query.iter_mut() {
            if *player_id == event.player_id {
                info!("Saving final position for player {:?}", player_id);
                let pool = db_pool.clone();
                let player_id_val = player_id.0 as i64;
                let x = position.x;
                let y = position.y;
                IoTaskPool::get().spawn(async move {
                    sqlx::query!(
                        "UPDATE players SET last_position_x = $1, last_position_y = $2, last_login_at = NOW() WHERE id = $3",
                        x, y, player_id_val
                    )
                    .execute(&pool)
                    .await
                    .ok();
                }).detach();
                
                commands.entity(entity).despawn();
                break;
            }
        }
    }
}

// Helper struct for mapping database rows
#[derive(FromRow)]
struct DbPlayer {
    id: i64,
    last_position_x: f32,
    last_position_y: f32,
}

