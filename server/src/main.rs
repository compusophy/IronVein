use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use bevy::{prelude::*, time::FixedTime};
use futures::{sink::SinkExt, StreamExt};
use shared::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use bincode;
use dotenvy::dotenv;
use sqlx::PgPool;
use std::env;
use tracing::{info, warn};

// A command sent from the web server to the Bevy app.
enum WorldCommand {
    ClientConnected {
        client_tx: mpsc::Sender<Message>,
    },
    ClientDisconnected {
        player_id: PlayerId,
    },
}

// Global state shared between Axum handlers.
#[derive(Clone, Resource)]
struct AppState {
    // Sends commands to the Bevy app
    world_cmd_tx: mpsc::Sender<WorldCommand>,
    // Sends client messages to the Bevy app
    client_msg_tx: mpsc::Sender<(PlayerId, ClientToServerMsg)>,
    // Database connection pool
    db_pool: PgPool,
}

#[derive(Event)]
struct PlayerDisconnect {
    player_id: PlayerId,
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    tracing_subscriber::fmt::init();

    // --- Database and Channel Setup ---
    let db_pool = PgPool::connect(&env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Failed to connect to database");

    let (world_cmd_tx, mut world_cmd_rx) = mpsc::channel(100);
    let (client_msg_tx, mut client_msg_rx) = mpsc::channel(1000);
    
    // --- Axum State ---
    let app_state = AppState {
        world_cmd_tx,
        client_msg_tx: client_msg_tx.clone(),
        db_pool,
    };
    
    // --- Bevy App Setup ---
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.insert_resource(FixedTime::new_from_secs(1.0 / 30.0)); // 30 times a second
    
    // Systems that run on a fixed timestep
    app.add_systems(
        FixedUpdate,
        (
            broadcast_world_state_system,
            move_players_system,
        ),
    );

    // Systems that run every frame
    app.add_systems(Update, 
        (
            handle_client_messages,
            handle_world_commands,
            save_player_position_on_disconnect,
        )
    );
    app.add_event::<PlayerDisconnect>();
    
    // Insert channels and other resources into the Bevy World
    app.insert_resource(mpsc::channel::<PlayerDisconnect>(100).0); // Event channel
    app.insert_resource(world_cmd_rx);
    app.insert_resource(client_msg_rx);
    app.insert_resource(app_state.clone());
    app.insert_resource(Clients::default());

    // --- Start Bevy and Axum ---
    let bevy_thread = std::thread::spawn(move || {
        app.run();
    });

    let router = Router::new()
        .route("/game", get(ws_handler))
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, router.into_make_service())
        .await
        .unwrap();

    bevy_thread.join().unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade, State(app_state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, app_state))
}

async fn handle_socket(socket: WebSocket, app_state: AppState) {
    info!("WebSocket connection established");
    let (mut sender, mut receiver) = socket.split();
    
    // Create a channel for this specific client to receive messages from Bevy
    let (client_tx, mut client_rx) = mpsc::channel(100);

    // Tell the Bevy world a new client has connected
    if app_state.world_cmd_tx.send(WorldCommand::ClientConnected { client_tx }).await.is_err() {
        warn!("Failed to send ClientConnected command to Bevy world");
        return;
    }
    
    let player_id_arc = Arc::new(tokio::sync::Mutex::new(None));

    // --- Two tasks to handle bidirectional communication ---

    // 1. Receives messages from the Bevy app and sends them to the client's WebSocket
    let mut send_task = tokio::spawn({
        let player_id_arc = player_id_arc.clone();
        async move {
            while let Some(msg) = client_rx.recv().await {
                 // Check if this is the Welcome message to get the player_id
                if let Message::Binary(ref bytes) = msg {
                    if let Ok(ServerToClientMsg::Welcome { player_id, .. }) = bincode::deserialize(bytes) {
                        *player_id_arc.lock().await = Some(player_id);
                    }
                }
                if sender.send(msg).await.is_err() {
                    break;
                }
            }
        }
    });

    // 2. Receives messages from the client's WebSocket and sends them to the Bevy app
    let client_msg_tx = app_state.client_msg_tx.clone();
    let mut recv_task = tokio::spawn({
        let player_id_arc = player_id_arc.clone();
        async move {
            while let Some(Ok(msg)) = receiver.next().await {
                if let Message::Binary(bytes) = msg {
                     if let Some(player_id) = *player_id_arc.lock().await {
                        if let Ok(client_msg) = bincode::deserialize(&bytes) {
                            if client_msg_tx.send((player_id, client_msg)).await.is_err() {
                                break; // Bevy app has disconnected
                            }
                        }
                    }
                }
            }
        }
    });
    
    // Wait for either task to finish
    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };

    info!("Client disconnected");
    if let Some(player_id) = *player_id_arc.lock().await {
        if app_state.world_cmd_tx.send(WorldCommand::ClientDisconnected { player_id }).await.is_err() {
            warn!("Failed to send ClientDisconnected for player {:?}", player_id);
        }
    }
}

#[derive(Resource, Default)]
struct Clients(Vec<mpsc::Sender<Message>>);

// This system runs in Bevy and processes commands from the web server
fn handle_world_commands(
    mut commands: Commands,
    mut world_cmd_rx: ResMut<mpsc::Receiver<WorldCommand>>,
    mut clients: ResMut<Clients>,
    db_pool: Res<AppState>,
    mut disconnect_tx: ResMut<mpsc::Sender<PlayerDisconnect>>,
) {
    while let Ok(cmd) = world_cmd_rx.try_recv() {
        match cmd {
            WorldCommand::ClientConnected { client_tx } => {
                info!("Received ClientConnected command");
                let client_id = clients.0.len();
                clients.0.push(client_tx.clone());
                
                let db_pool = db_pool.db_pool.clone();

                // Spawn a task to handle the database interaction
                commands.spawn(async move {
                    let username = format!("Player_{}", client_id);
                     let user_record = sqlx::query!("SELECT id, last_position_x, last_position_y FROM players WHERE username = $1", username)
                        .fetch_optional(&db_pool)
                        .await
                        .unwrap();

                    let (player_id, start_pos) = if let Some(record) = user_record {
                        (PlayerId(record.id as u64), Position { x: record.last_position_x, y: record.last_position_y })
                    } else {
                        let new_record = sqlx::query!("INSERT INTO players (username, password_hash) VALUES ($1, $2) RETURNING id, last_position_x, last_position_y", username, "password")
                            .fetch_one(&db_pool)
                            .await
                            .unwrap();
                        (PlayerId(new_record.id as u64), Position { x: new_record.last_position_x, y: new_record.last_position_y })
                    };
                    
                    // This is a command to run on the main Bevy thread
                    (player_id, start_pos, client_tx)
                }.then(move |(player_id, start_pos, client_tx)| async move {
                    // This closure will be run by Bevy on the main thread
                    commands.spawn((
                        Player,
                        player_id,
                        start_pos,
                        TargetDestination { x: start_pos.x, y: start_pos.y },
                    ));

                     // Send Welcome message
                    let welcome_msg = ServerToClientMsg::Welcome { player_id, config: GameConfig { map_width: 800, map_height: 600 } };
                    let welcome_msg_binary = bincode::serialize(&welcome_msg).unwrap();
                    if client_tx.send(Message::Binary(welcome_msg_binary)).await.is_err() {
                        warn!("Failed to send Welcome message to client");
                    }

                    // Broadcast PlayerJoined message
                    let joined_msg = ServerToClientMsg::PlayerJoined(player_id, start_pos);
                    let joined_msg_binary = bincode::serialize(&joined_msg).unwrap();
                    broadcast_message(clients.as_ref(), Message::Binary(joined_msg_binary), Some(client_id)).await;
                }));
            }
            WorldCommand::ClientDisconnected { player_id } => {
                info!("Received ClientDisconnected for player {:?}", player_id);
                disconnect_tx.try_send(PlayerDisconnect { player_id }).ok();
            }
        }
    }
}

fn handle_client_messages(
    mut query: Query<(&PlayerId, &mut TargetDestination)>,
    mut client_msg_rx: ResMut<mpsc::Receiver<(PlayerId, ClientToServerMsg)>>,
) {
    while let Ok((player_id, msg)) = client_msg_rx.try_recv() {
        match msg {
            ClientToServerMsg::ClickPosition { x, y } => {
                for (p_id, mut target_dest) in query.iter_mut() {
                    if *p_id == player_id {
                        target_dest.x = x;
                        target_dest.y = y;
                        break;
                    }
                }
            }
            ClientToServerMsg::Ping => {}
        }
    }
}

fn move_players_system(mut query: Query<(&mut Position, &TargetDestination)>) {
    for (mut pos, target) in query.iter_mut() {
        let dx = target.x - pos.x;
        let dy = target.y - pos.y;
        if dx.abs() > 0.1 || dy.abs() > 0.1 {
            pos.x += dx * 0.1;
            pos.y += dy * 0.1;
        }
    }
}

fn broadcast_world_state_system(
    query: Query<(&PlayerId, &Position)>,
    clients: Res<Clients>,
) {
    let players: Vec<(PlayerId, Position)> = query.iter().map(|(pid, pos)| (*pid, *pos)).collect();
    let msg = ServerToClientMsg::WorldStateSnapshot(players);
    let binary_msg = bincode::serialize(&msg).unwrap();

    let clients = clients.into_inner();
    tokio::spawn(async move {
        broadcast_message(clients, Message::Binary(binary_msg), None).await;
    });
}

async fn broadcast_message(
    clients: &Clients,
    msg: Message,
    exclude_id: Option<usize>,
) {
    let mut dead_clients = Vec::new();
    for (i, client) in clients.0.iter().enumerate() {
        if exclude_id.map_or(true, |id| i != id) {
            if client.send(msg.clone()).await.is_err() {
                dead_clients.push(i);
            }
        }
    }
    // Note: In a real app, you'd need a way to remove dead clients from the main resource.
    // This is simplified for this example.
}

fn save_player_position_on_disconnect(
    mut events: EventReader<PlayerDisconnect>,
    query: Query<(&PlayerId, &Position)>,
    app_state: Res<AppState>,
    mut commands: Commands,
) {
    for event in events.read() {
        for (player_id, position) in query.iter() {
            if *player_id == event.player_id {
                info!("Saving position for disconnected player {:?}", player_id);
                let db_pool = app_state.db_pool.clone();
                let player_id_val = player_id.0 as i64;
                let x = position.x;
                let y = position.y;
                commands.spawn(async move {
                    sqlx::query!(
                        "UPDATE players SET last_position_x = $1, last_position_y = $2, last_login_at = NOW() WHERE id = $3",
                        x, y, player_id_val
                    )
                    .execute(&db_pool)
                    .await
                    .ok();
                });
                
                // Also despawn the entity
                if let Some(entity) = query.iter().find(|(pid, _)| **pid == event.player_id).map(|(e, _)| e) {
                    commands.entity(entity).despawn();
                }
                break;
            }
        }
    }
}
