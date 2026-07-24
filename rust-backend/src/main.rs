mod brain;
mod config;
mod detail;
mod execution;
mod models;
mod readiness;
mod replay;
mod scanner;
mod state;
mod stream;
mod telegram;

use axum::{
    http::{header, HeaderValue},
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use config::SETTINGS;
use detail::build_symbol_detail;
use serde_json::{json, Value};
use socketioxide::extract::{AckSender, Data, SocketRef};
use socketioxide::SocketIo;
use state::STATE;
use std::time::Duration;

const DASHBOARD_HTML: &str = include_str!("../dashboard/index.html");
const DASHBOARD_CSS: &str = include_str!("../dashboard/app.css");
const DASHBOARD_JS: &str = include_str!("../dashboard/app.js");

async fn index() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn dashboard_css() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        DASHBOARD_CSS,
    )
}

async fn dashboard_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        DASHBOARD_JS,
    )
}

async fn health() -> Json<Value> {
    let state = STATE.lock();
    Json(json!({
        "ok": true,
        "source": state.source,
        "connections": state.connections,
        "engine": "rust",
    }))
}

async fn snapshot() -> Json<Value> {
    Json(STATE.lock().snapshot())
}

fn normalize_symbol(payload: &Value) -> Option<String> {
    let raw = payload
        .as_str()
        .map(str::to_string)
        .or_else(|| payload["symbol"].as_str().map(str::to_string))?;
    let symbol = raw.trim().to_uppercase();
    if symbol.is_empty() {
        return None;
    }
    // Reject symbols outside the currently tracked market universe.
    if !STATE.lock().books.contains_key(&symbol) {
        return None;
    }
    Some(symbol)
}

fn symbol_room(symbol: &str) -> String {
    format!("symbol:{symbol}")
}

async fn on_connect(socket: SocketRef) {
    let snapshot = STATE.lock().snapshot();
    let _ = socket.emit("snapshot", &snapshot);

    socket.on(
        "subscribe_symbol",
        async |socket: SocketRef, Data::<Value>(payload), ack: AckSender| {
            let Some(symbol) = normalize_symbol(&payload) else {
                let _ = ack.send(&json!({"ok": false, "error": "Unknown symbol"}));
                return;
            };
            socket.join(symbol_room(&symbol));
            // First detailed payload immediately, instead of waiting for the
            // next background broadcast.
            let detail = {
                let state = STATE.lock();
                build_symbol_detail(&state, &symbol)
            };
            if let Some(detail) = detail {
                let _ = socket.emit("symbol_detail", &detail);
            }
            let _ = ack.send(&json!({"ok": true, "symbol": symbol}));
        },
    );

    socket.on(
        "unsubscribe_symbol",
        async |socket: SocketRef, Data::<Value>(payload), ack: AckSender| {
            let Some(symbol) = normalize_symbol(&payload) else {
                let _ = ack.send(&json!({"ok": false, "error": "Unknown symbol"}));
                return;
            };
            socket.leave(symbol_room(&symbol));
            let _ = ack.send(&json!({"ok": true, "symbol": symbol}));
        },
    );
}

async fn broadcast_loop(io: SocketIo) {
    loop {
        let snapshot = STATE.lock().snapshot();
        let _ = io.emit("snapshot", &snapshot).await;

        // One detailed payload per subscribed symbol room.
        let symbols: Vec<String> = STATE.lock().books.keys().cloned().collect();
        for symbol in symbols {
            let room = symbol_room(&symbol);
            let sockets = io.to(room.clone()).sockets();
            if sockets.is_empty() {
                continue;
            }
            let detail = {
                let state = STATE.lock();
                build_symbol_detail(&state, &symbol)
            };
            if let Some(detail) = detail {
                let _ = io.to(room).emit("symbol_detail", &detail).await;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn engine_loop() {
    let mut manager = stream::StreamManager::new();
    if SETTINGS.force_demo {
        let symbols: Vec<String> = SETTINGS
            .demo_symbols
            .iter()
            .map(|s| s.to_string())
            .collect();
        manager.set_symbols(symbols, true).await;
    }
    scanner::radar_loop(&mut manager).await;
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();
    SETTINGS.assert_safe_mode();
    telegram::start(&SETTINGS.trading_mode);

    let (socketio_layer, io) = SocketIo::new_layer();
    io.ns("/", on_connect);

    tokio::spawn(engine_loop());
    tokio::spawn(brain::brain_loop());
    tokio::spawn(broadcast_loop(io.clone()));

    let app = Router::new()
        .route("/", get(index))
        .route("/static/app.css", get(dashboard_css))
        .route("/static/app.js", get(dashboard_js))
        .route("/api/health", get(health))
        .route("/api/snapshot", get(snapshot))
        .layer(socketio_layer);

    let address = format!("{}:{}", SETTINGS.host, SETTINGS.port);
    println!(
        "PulseBook (rust) listening on http://{address} · mode={} · LIVE TRADING LOCKED",
        SETTINGS.trading_mode
    );
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .expect("bind server port");
    axum::serve(listener, app).await.expect("server crashed");
}
