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

use axum::{
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use config::{admin_password, save_settings, Settings, SETTINGS};
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

fn supplied_admin_password(headers: &HeaderMap) -> Option<&str> {
    if let Some(password) = headers
        .get("x-admin-password")
        .and_then(|value| value.to_str().ok())
    {
        return Some(password.trim());
    }

    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|authorization| {
            authorization
                .strip_prefix("Bearer ")
                .or_else(|| authorization.strip_prefix("bearer "))
                .or(Some(authorization))
        })
        .map(str::trim)
}

fn admin_authorized(headers: &HeaderMap) -> bool {
    let Some(expected) = admin_password() else {
        return false;
    };
    let Some(supplied) = supplied_admin_password(headers) else {
        return false;
    };
    let expected = expected.trim();
    if supplied.len() != expected.len() {
        return false;
    }
    supplied
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

async fn settings_status() -> Json<Value> {
    // A single guard: nested STATE.lock() calls inside one expression
    // deadlock because parking_lot mutexes are not reentrant.
    let (source, active_symbols, selected_symbols) = {
        let state = STATE.lock();
        (state.source.clone(), state.books.len(), state.radar.len())
    };
    Json(json!({
        "configured": admin_password().is_some(),
        "mode": SETTINGS.trading_mode,
        "source": source,
        "active_symbols": active_symbols,
        "selected_symbols": selected_symbols,
        "max_symbols": SETTINGS.max_symbols,
        "config_path": config::config_path().display().to_string(),
        "live_trading_locked": true
    }))
}

async fn get_settings(headers: HeaderMap) -> Result<Json<Settings>, (StatusCode, Json<Value>)> {
    if !admin_authorized(&headers) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Неверный пароль администратора"})),
        ));
    }
    Ok(Json(SETTINGS.clone()))
}

async fn put_settings(
    headers: HeaderMap,
    Json(settings): Json<Settings>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !admin_authorized(&headers) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Неверный пароль администратора"})),
        ));
    }
    save_settings(&settings)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({"error": error}))))?;
    Ok(Json(
        json!({"ok": true, "restart_required": true, "message": "Настройки сохранены. Перезапустите движок для применения."}),
    ))
}

async fn restart_engine(headers: HeaderMap) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !admin_authorized(&headers) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Неверный пароль администратора"})),
        ));
    }
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        // A non-zero dedicated code makes both systemd Restart=on-failure and PM2 restart us.
        std::process::exit(75);
    });
    Ok(Json(
        json!({"ok": true, "message": "Движок перезапускается менеджером процессов"}),
    ))
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
        .route("/api/settings/status", get(settings_status))
        .route("/api/admin/settings", get(get_settings).put(put_settings))
        .route("/api/admin/restart", post(restart_engine))
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

#[cfg(test)]
mod tests {
    use super::{supplied_admin_password, DASHBOARD_CSS, DASHBOARD_HTML, DASHBOARD_JS};
    use axum::http::{header, HeaderMap, HeaderValue};

    #[test]
    fn accepts_supported_admin_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-admin-password",
            HeaderValue::from_static("secret-password"),
        );
        assert_eq!(supplied_admin_password(&headers), Some("secret-password"));

        headers.remove("x-admin-password");
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-password"),
        );
        assert_eq!(supplied_admin_password(&headers), Some("secret-password"));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("secret-password"),
        );
        assert_eq!(supplied_admin_password(&headers), Some("secret-password"));
    }

    #[test]
    fn dashboard_assets_are_embedded() {
        assert!(DASHBOARD_HTML.contains("PulseBook"));
        assert!(DASHBOARD_HTML.contains("/static/app.css"));
        assert!(DASHBOARD_HTML.contains("/static/app.js"));
        assert!(DASHBOARD_CSS.contains(":root"));
        assert!(DASHBOARD_JS.contains("/api/snapshot"));
        assert!(!DASHBOARD_HTML.contains("dashboard template missing"));
    }
}
