use bevy::prelude::*;
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::{futures::WebSocket, Message};
use shared::{ServerToClientMsg, ClientToServerMsg, PlayerId, Position, GameConfig};

struct Connection(WebSocket);
#[derive(Resource)]
struct LocalPlayerId(PlayerId);
#[derive(Resource)]
struct GameConfigRes(GameConfig);

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .insert_resource(ClearColor(Color::srgb(0.1, 0.2, 0.8)))
        .add_systems(Startup, (setup, web_socket_setup))
        .add_systems(Update, (handle_web_socket_messages, mouse_click_system))
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn(Camera2dBundle::default());
}

fn web_socket_setup(mut commands: Commands) {
    let ws = WebSocket::open("ws://127.0.0.1:3000/game").unwrap();
    commands.insert_resource(Connection(ws));
}

fn mouse_click_system(
    buttons: Res<Input<MouseButton>>,
    windows: Query<&Window>,
    mut conn: ResMut<Connection>,
) {
    let window = windows.single();
    if buttons.just_pressed(MouseButton::Left) {
        if let Some(position) = window.cursor_position() {
            let msg = ClientToServerMsg::ClickPosition {
                x: position.x - window.width() / 2.0,
                y: -(position.y - window.height() / 2.0),
            };
            let binary_msg = bincode::serialize(&msg).unwrap();
            conn.0.try_send(Message::Bytes(binary_msg)).unwrap();
            info!("Sent click position: {:?}", msg);
        }
    }
}

fn handle_web_socket_messages(
    mut commands: Commands, 
    mut conn: ResMut<Connection>, 
    local_player: Option<Res<LocalPlayerId>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut player_query: Query<(Entity, &PlayerId, &mut Position, &mut Transform)>,
) {
    while let Some(Ok(Message::Bytes(bytes))) = conn.0.try_next() {
        if let Ok(msg) = bincode::deserialize::<ServerToClientMsg>(&bytes) {
            info!("Received message: {:?}", msg);
            match msg {
                ServerToClientMsg::Welcome { player_id, config } => {
                    commands.insert_resource(LocalPlayerId(player_id));
                    commands.insert_resource(GameConfigRes(config));
                }
                ServerToClientMsg::PlayerJoined(player_id, position) => {
                    spawn_player_sprite(&mut commands, &mut materials, player_id, position, false);
                }
                ServerToClientMsg::PlayerLeft(player_id) => {
                    for (entity, p_id, _, _) in player_query.iter_mut() {
                        if *p_id == player_id {
                            commands.entity(entity).despawn();
                            break;
                        }
                    }
                }
                ServerToClientMsg::WorldStateSnapshot(players) => {
                    let mut existing_players: std::collections::HashMap<PlayerId, (Entity, Mut<Position>, Mut<Transform>)> =
                        player_query.iter_mut().map(|(e, pid, p, t)| (*pid, (e, p, t))).collect();

                    for (player_id, position) in players {
                        if let Some((_, mut pos, mut transform)) = existing_players.remove(&player_id) {
                            pos.x = position.x;
                            pos.y = position.y;
                            transform.translation.x = position.x;
                            transform.translation.y = position.y;
                        } else {
                            let is_local_player = local_player.as_ref().map_or(false, |p| p.0 == player_id);
                            spawn_player_sprite(&mut commands, &mut materials, player_id, position, is_local_player);
                        }
                    }
                }
                ServerToClientMsg::Pong => {}
            }
        } else {
            warn!("Failed to deserialize message");
        }
    }
}

fn spawn_player_sprite(
    commands: &mut Commands,
    materials: &mut ResMut<Assets<ColorMaterial>>,
    player_id: PlayerId,
    position: Position,
    is_local_player: bool,
) {
    let color = if is_local_player {
        Color::srgb(0.0, 0.0, 1.0)
    } else {
        Color::srgb(1.0, 0.0, 0.0)
    };

    commands.spawn((
        SpriteBundle {
            sprite: Sprite {
                color,
                custom_size: Some(Vec2::new(10.0, 10.0)),
                ..default()
            },
            transform: Transform::from_xyz(position.x, position.y, 0.0),
            ..default()
        },
        player_id,
        position,
    ));
}
