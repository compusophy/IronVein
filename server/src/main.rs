use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use bevy::prelude::*;
use futures::{sink::SinkExt, stream::StreamExt};
use shared::*;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};
use bincode;
use dotenvy::dotenv;
use sqlx::PgPool;
use std::env;

// AppState that holds the Bevy World and connected clients
#[derive(Clone, Resource)]
struct AppState {
    clients: Arc<Mutex<Vec<mpsc::Sender<Message>>>>,
    player_id_counter: Arc<Mutex<u64>>,
    client_msg_rx: Arc<Mutex<mpsc::Receiver<(PlayerId, ClientToServerMsg)>>>,
    client_msg_tx: mpsc::Sender<(PlayerId, ClientToServerMsg)>,
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

    let db_pool = PgPool::connect(&env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Failed to connect to database");

    let (client_msg_tx, client_msg_rx) = mpsc::channel(1000);

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_systems(Update, (player_spawn_system, handle_client_messages, move_players_system, save_player_position_on_disconnect, despawn_disconnected_players));
    app.add_systems(
        FixedUpdate,
        broadcast_world_state_system,
    );
    app.insert_resource(FixedTime::new_from_secs(1.0 / 30.0)); // 30 times a second
    app.add_event::<PlayerDisconnect>();

    let app_state = AppState {
        clients: Arc::new(Mutex::new(Vec::new())),
        player_id_counter: Arc::new(Mutex::new(0)),
        client_msg_rx: Arc::new(Mutex::new(client_msg_rx)),
        client_msg_tx,
        db_pool,
    };
    app.insert_resource(app_state);

    let world = Arc::new(Mutex::new(app.world));

    // Run Bevy app in a separate thread
    let bevy_thread = std::thread::spawn(move || {
        app.run();
    });

    let router = Router::new()
        .route("/game", get(ws_handler))
        .with_state(world);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, router.into_make_service())
        .await
        .unwrap();

    bevy_thread.join().unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade, State(world): State<Arc<Mutex<World>>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, world))
}

async fn handle_socket(socket: WebSocket, world: Arc<Mutex<World>>) {
    info!("WebSocket connection established");
    let (mut sender, mut receiver) = socket.split();

    let app_state = world.lock().unwrap().resource::<AppState>().clone();
    let db_pool = app_state.db_pool.clone();

    // Mock login/signup
    // In a real app, you'd get username/password from the client
    let username = format!("Player_{}", app_state.clients.lock().unwrap().len());
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

    let (tx, mut rx) = mpsc::channel(100);
    app_state.clients.lock().unwrap().push(tx.clone());
    let client_id = app_state.clients.lock().unwrap().len() - 1;

    // Spawn player entity in Bevy world
    world.lock().unwrap().spawn((
        Player,
        player_id,
        start_pos,
        TargetDestination { x: start_pos.x, y: start_pos.y },
    ));
    info!("Spawned player with ID: {:?}", player_id);

    // Send Welcome message
    let welcome_msg = ServerToClientMsg::Welcome {
        player_id,
        config: GameConfig {
            map_width: 800,
            map_height: 600,
        },
    };
    let welcome_msg_binary = bincode::serialize(&welcome_msg).unwrap();
    if tx.send(Message::Binary(welcome_msg_binary)).await.is_err() {
        warn!("Failed to send Welcome message to client {}", client_id);
    }

    // Broadcast PlayerJoined message
    let joined_msg = ServerToClientMsg::PlayerJoined(player_id, start_pos);
    let joined_msg_binary = bincode::serialize(&joined_msg).unwrap();
    broadcast_message(
        app_state.clients.clone(),
        Message::Binary(joined_msg_binary),
        Some(client_id),
    )
    .await;

    // Forward messages from the channel to the client
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Handle incoming messages from the client
    let recv_tx = app_state.client_msg_tx.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Binary(b) => {
                    if let Ok(msg) = bincode::deserialize::<ClientToServerMsg>(&b) {
                        if recv_tx.send((player_id, msg)).await.is_err() {
                            warn!("Failed to send client message to bevy app");
                        }
                    } else {
                        warn!("Failed to deserialize message");
                    }
                }
                Message::Close(_) => {
                    info!("Client disconnected");
                    break;
                }
                _ => {}
            }
        }
    });
    
    // Wait for either task to finish
    tokio::select! {
        _ = (&mut send_task) => {},
        _ = (&mut recv_task) => {},
    };

    info!("Client {} disconnected", client_id);
    world.lock().unwrap().send_event(PlayerDisconnect { player_id });
    app_state.clients.lock().unwrap().remove(client_id);
}

fn player_spawn_system(query: Query<(Entity, &PlayerId), Added<Player>>) {
    for (entity, player_id) in query.iter() {
        info!("A player with id {:?} has been spawned with entity {:?}.", player_id, entity);
    }
}

fn handle_client_messages(
    mut query: Query<(&PlayerId, &mut TargetDestination)>,
    app_state: Res<AppState>,
) {
    let mut rx = app_state.client_msg_rx.lock().unwrap();
    while let Ok((player_id, msg)) = rx.try_recv() {
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
    app_state: Res<AppState>,
) {
    let players: Vec<(PlayerId, Position)> = query.iter().map(|(pid, pos)| (*pid, *pos)).collect();
    let msg = ServerToClientMsg::WorldStateSnapshot(players);
    let binary_msg = bincode::serialize(&msg).unwrap();

    let clients = app_state.clients.clone();
    tokio::spawn(async move {
        broadcast_message(clients, Message::Binary(binary_msg), None).await;
    });
}

async fn broadcast_message(
    clients: Arc<Mutex<Vec<mpsc::Sender<Message>>>>,
    msg: Message,
    exclude_id: Option<usize>,
) {
    let clients = clients.lock().unwrap();
    for (i, client) in clients.iter().enumerate() {
        if exclude_id.map_or(true, |id| i != id) {
            if client.send(msg.clone()).await.is_err() {
                warn!("Failed to send message to client {}", i);
            }
        }
    }
}

fn save_player_position_on_disconnect(
    mut events: EventReader<PlayerDisconnect>,
    query: Query<(&PlayerId, &Position)>,
    app_state: Res<AppState>,
) {
    for ev in events.read() {
        for (player_id, position) in query.iter() {
            if *player_id == ev.player_id {
                let db_pool = app_state.db_pool.clone();
                let pid = player_id.0 as i64;
                let pos = position.clone();
                tokio::spawn(async move {
                    sqlx::query!(
                        "UPDATE players SET last_position_x = $1, last_position_y = $2 WHERE id = $3",
                        pos.x,
                        pos.y,
                        pid
                    )
                    .execute(&db_pool)
                    .await
                    .unwrap();
                });
                break;
            }
        }
    }
}

fn despawn_disconnected_players(
    mut commands: Commands,
    mut events: EventReader<PlayerDisconnect>,
    query: Query<(Entity, &PlayerId)>,
) {
    for ev in events.read() {
        for (entity, player_id) in query.iter() {
            if *player_id == ev.player_id {
                commands.entity(entity).despawn();
                break;
            }
        }
    }
}
