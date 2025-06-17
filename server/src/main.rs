use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use bevy::{
    prelude::*,
    tasks::{IoTaskPool, Task},
};
use futures_util::{future::FutureExt, stream::StreamExt, SinkExt};
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
    Connect { client_tx: mpsc::Sender<Message> },
    Disconnect { client_id: usize },
}

/// A message sent from a client's websocket to the Bevy world.
struct ClientMessage {
    client_id: usize,
    message: ClientToServerMsg,
}

/// Axum's state, shared with all websocket handlers.
#[derive(Clone)]
struct AppState {
    world_cmd_tx: mpsc::Sender<WorldCommand>,
    client_msg_tx: mpsc::Sender<ClientMessage>,
}

// --- Bevy Resources ---

#[derive(Resource)]
struct WorldCommandRx(mpsc::Receiver<WorldCommand>);

#[derive(Resource)]
struct ClientMessageRx(mpsc::Receiver<ClientMessage>);

#[derive(Resource, Deref, DerefMut, Default, Clone)]
struct Clients(Vec<Option<mpsc::Sender<Message>>>);

#[derive(Resource, Deref, DerefMut)]
struct DbPool(PgPool);

#[derive(Event)]
struct PlayerDisconnectEvent {
    player_id: PlayerId,
}

// --- Main Application Setup ---

#[tokio::main]
async fn main() {
    dotenv().ok();
    tracing_subscriber::fmt::init();

    let db_pool = PgPool::connect(&env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Failed to connect to database");
    info!("Database connection established.");

    let (world_cmd_tx, world_cmd_rx) = mpsc::channel(100);
    let (client_msg_tx, client_msg_rx) = mpsc::channel(1000);

    let bevy_thread = std::thread::spawn(move || {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::core::CorePlugin));
        app.insert_resource(Clients::default());
        app.insert_resource(WorldCommandRx(world_cmd_rx));
        app.insert_resource(ClientMessageRx(client_msg_rx));
        app.insert_resource(DbPool(db_pool));
        app.add_event::<PlayerDisconnectEvent>();
        app.add_systems(
            Update,
            (
                handle_world_commands,
                handle_client_messages,
                save_player_position_on_disconnect,
                despawn_disconnected_players,
                apply_deferred,
            )
                .chain(),
        );
        app.add_systems(FixedUpdate, (move_players_system, broadcast_world_state_system));
        info!("Starting Bevy app...");
        app.run();
    });

    let axum_state = AppState {
        world_cmd_tx,
        client_msg_tx,
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

    let (client_tx, mut client_rx) = mpsc::channel(100);

    if state
        .world_cmd_tx
        .send(WorldCommand::Connect { client_tx })
        .await
        .is_err()
    {
        warn!("World command channel closed, cannot connect client.");
        return;
    }

    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    let client_id_arc = Arc::new(tokio::sync::Mutex::new(None::<usize>));
    let mut recv_task = tokio::spawn({
        let state = state.clone();
        let client_id_arc = client_id_arc.clone();
        async move {
            while let Some(Ok(msg)) = ws_receiver.next().await {
                match msg {
                    Message::Binary(bytes) => {
                        if let Ok(client_msg) = bincode::deserialize::<ClientToServerMsg>(&bytes) {
                             if let Some(client_id) = *client_id_arc.lock().await {
                                let msg = ClientMessage { client_id, message: client_msg };
                                if state.client_msg_tx.send(msg).await.is_err() {
                                    break;
                                }
                             }
                        }
                    },
                    Message::Text(text) => {
                         if let Ok(client_id) = text.parse::<usize>() {
                            *client_id_arc.lock().await = Some(client_id);
                         }
                    }
                    _ => {}
                }
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    if let Some(client_id) = *client_id_arc.lock().await {
        if state
            .world_cmd_tx
            .send(WorldCommand::Disconnect { client_id })
            .await
            .is_err()
        {
            warn!("World command channel closed, could not disconnect client {}", client_id);
        }
    }
    info!("WebSocket connection closed");
}

// --- Bevy Systems ---

type PlayerDbInfoTask = Task<(PlayerId, Position)>;

#[derive(Component)]
struct NewPlayerDbInfo(PlayerDbInfoTask);

/// Handles commands from Axum, like spawning a player entity after a DB query.
fn handle_world_commands(
    mut commands: Commands,
    mut world_cmd_rx: ResMut<WorldCommandRx>,
    mut clients: ResMut<Clients>,
    db_pool: Res<DbPool>,
    mut new_players: Query<(Entity, &mut NewPlayerDbInfo)>,
) {
    // Handle completed DB queries for new players
    for (entity, mut task) in new_players.iter_mut() {
        if let Some((player_id, start_pos)) = task.0.now_or_never() {
            let client_id_val = player_id.0 as usize;
             if let Some(Some(client_tx)) = clients.get(client_id_val).cloned() {
                // Send Welcome message
                let welcome_msg = ServerToClientMsg::Welcome { player_id, config: GameConfig { map_width: 800, map_height: 600 } };
                let welcome_msg_binary = bincode::serialize(&welcome_msg).unwrap();
                client_tx.try_send(Message::Binary(welcome_msg_binary)).ok();

                // Broadcast PlayerJoined
                let joined_msg = ServerToClientMsg::PlayerJoined(player_id, start_pos);
                let joined_msg_binary = bincode::serialize(&joined_msg).unwrap();
                let clients = clients.clone();
                IoTaskPool::get().spawn(async move {
                    broadcast_message(&clients, Message::Binary(joined_msg_binary), Some(client_id_val)).await;
                }).detach();

                // Add components to the player entity
                commands.entity(entity).insert((
                    Player,
                    player_id,
                    start_pos,
                    TargetDestination(start_pos),
                ));
             }
             commands.entity(entity).remove::<NewPlayerDbInfo>();
        }
    }

    // Handle new connections
    while let Ok(cmd) = world_cmd_rx.0.try_recv() {
        match cmd {
            WorldCommand::Connect { client_tx } => {
                let client_id = clients.len();
                clients.push(Some(client_tx.clone()));
                client_tx.try_send(Message::Text(client_id.to_string())).ok();

                let pool = db_pool.0.clone();
                let task = IoTaskPool::get().spawn(async move {
                    let username = format!("Player_{}", client_id);
                    let user_record = sqlx::query_as::<_, DbPlayer>("SELECT id, last_position_x, last_position_y FROM players WHERE username = $1")
                        .bind(&username)
                        .fetch_optional(&pool)
                        .await.ok().flatten();

                    if let Some(player) = user_record {
                        (PlayerId(player.id as u64), Position { x: player.last_position_x, y: player.last_position_y })
                    } else {
                        let new_player = sqlx::query_as::<_, DbPlayer>("INSERT INTO players (username, password_hash) VALUES ($1, $2) RETURNING id, last_position_x, last_position_y")
                            .bind(&username).bind("password").fetch_one(&pool).await.unwrap();
                        (PlayerId(new_player.id as u64), Position { x: new_player.last_position_x, y: new_player.last_position_y })
                    }
                });
                commands.spawn(NewPlayerDbInfo(task));
            }
            WorldCommand::Disconnect { client_id } => {
                clients[client_id] = None;
            }
        }
    }
}

/// Handles incoming messages from all connected clients.
fn handle_client_messages(
    mut query: Query<(&PlayerId, &mut TargetDestination)>,
    mut client_msg_rx: ResMut<ClientMessageRx>,
    clients: Res<Clients>,
) {
    while let Ok(ClientMessage { client_id, message }) = client_msg_rx.0.try_recv() {
        if clients.get(client_id).is_none() { continue; } // Disconnected

        let player_id = query.iter().find_map(|(pid, _)| if pid.0 as usize == client_id { Some(*pid) } else {None});
        if let Some(player_id) = player_id {
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
fn broadcast_world_state_system(query: Query<(&PlayerId, &Position)>, clients: Res<Clients>) {
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
async fn broadcast_message(clients: &Clients, msg: Message, exclude_id: Option<usize>) {
    for (i, client_opt) in clients.iter().enumerate() {
        if let Some(client) = client_opt {
            if exclude_id.map_or(true, |id| i != id) {
                if client.send(msg.clone()).await.is_err() {
                    // Disconnected
                }
            }
        }
    }
}

/// On disconnect, saves the player's last position.
fn save_player_position_on_disconnect(
    mut events: EventReader<PlayerDisconnectEvent>,
    query: Query<(Entity, &PlayerId, &Position)>,
    db_pool: Res<DbPool>,
    mut commands: Commands,
) {
    for event in events.read() {
        for (entity, player_id, position) in query.iter() {
            if *player_id == event.player_id {
                info!("Saving final position for player {:?}", player_id);
                let pool = db_pool.0.clone();
                let player_id_val = player_id.0 as i64;
                let x = position.x;
                let y = position.y;
                IoTaskPool::get().spawn(async move {
                    sqlx::query("UPDATE players SET last_position_x = $1, last_position_y = $2, last_login_at = NOW() WHERE id = $3")
                        .bind(x)
                        .bind(y)
                        .bind(player_id_val)
                        .execute(&pool).await.ok();
                }).detach();
                commands.entity(entity).despawn();
                break;
            }
        }
    }
}

/// Removes entities for players that have disconnected.
fn despawn_disconnected_players(
    mut commands: Commands,
    query: Query<(Entity, &PlayerId)>,
    clients: Res<Clients>,
) {
    for (entity, player_id) in query.iter() {
        if clients.get(player_id.0 as usize).and_then(|c| c.as_ref()).is_none() {
            commands.entity(entity).despawn();
        }
    }
}

// Helper struct for mapping database rows
#[derive(FromRow, Clone)]
struct DbPlayer {
    id: i64,
    last_position_x: f32,
    last_position_y: f32,
}

