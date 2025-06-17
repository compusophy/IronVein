use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{WebSocket, MessageEvent, HtmlCanvasElement, CanvasRenderingContext2d, MouseEvent, Event};
use shared::{ServerToClientMsg, ClientToServerMsg, PlayerId, Position};
use std::collections::HashMap;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

macro_rules! console_log {
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

struct GameState {
    players: HashMap<PlayerId, Position>,
    local_player_id: Option<PlayerId>,
    canvas: HtmlCanvasElement,
    context: CanvasRenderingContext2d,
    ws: WebSocket,
}

#[wasm_bindgen(start)]
pub fn main() {
    console_error_panic_hook::set_once();
    
    let window = web_sys::window().unwrap();
    let document = window.document().unwrap();
    let canvas = document
        .create_element("canvas")
        .unwrap()
        .dyn_into::<HtmlCanvasElement>()
        .unwrap();
    
    canvas.set_width(800);
    canvas.set_height(600);
    canvas.style().set_property("border", "1px solid black").unwrap();
    
    let body = document.body().unwrap();
    body.append_child(&canvas).unwrap();
    
    let context = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into::<CanvasRenderingContext2d>()
        .unwrap();
    
    // Setup WebSocket connection
    let ws = WebSocket::new("wss://defective-crowd-production.up.railway.app/game").unwrap();
    
    let mut game_state = GameState {
        players: HashMap::new(),
        local_player_id: None,
        canvas: canvas.clone(),
        context,
        ws: ws.clone(),
    };
    
    // Setup WebSocket event handlers
    let onmessage_callback = Closure::wrap(Box::new(move |e: MessageEvent| {
        console_log!("Message received");
        // Handle WebSocket messages here
    }) as Box<dyn FnMut(_)>);
    
    ws.set_onmessage(Some(onmessage_callback.as_ref().unchecked_ref()));
    onmessage_callback.forget();
    
    let onopen_callback = Closure::wrap(Box::new(move |_e: web_sys::Event| {
        console_log!("WebSocket connection opened");
    }) as Box<dyn FnMut(_)>);
    
    ws.set_onopen(Some(onopen_callback.as_ref().unchecked_ref()));
    onopen_callback.forget();
    
    // Setup mouse click handler
    let onclick_callback = Closure::wrap(Box::new(move |e: MouseEvent| {
        console_log!("Canvas clicked at: {}, {}", e.offset_x(), e.offset_y());
        // Send click position to server
    }) as Box<dyn FnMut(_)>);
    
    canvas.set_onclick(Some(onclick_callback.as_ref().unchecked_ref()));
    onclick_callback.forget();
    
    // Initial render
    render_game(&game_state);
}

fn render_game(game_state: &GameState) {
    let ctx = &game_state.context;
    
    // Clear canvas
    ctx.set_fill_style(&wasm_bindgen::JsValue::from_str("lightblue"));
    ctx.fill_rect(0.0, 0.0, 800.0, 600.0);
    
    // Draw players
    for (player_id, position) in &game_state.players {
        let color = if Some(*player_id) == game_state.local_player_id {
            "blue"
        } else {
            "red"
        };
        
        ctx.set_fill_style(&wasm_bindgen::JsValue::from_str(color));
        ctx.begin_path();
        ctx.arc(
            position.x as f64 + 400.0, // Center offset
            position.y as f64 + 300.0, // Center offset
            5.0,
            0.0,
            2.0 * std::f64::consts::PI,
        ).unwrap();
        ctx.fill();
    }
}
