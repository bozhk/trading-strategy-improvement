use crate::config::SETTINGS;
use crate::models::{now_ts, OrderBook, TradeTick};
use crate::recorder;
use crate::state::STATE;
use futures_util::{SinkExt, StreamExt};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

pub struct StreamManager {
    tasks: Vec<JoinHandle<()>>,
    symbols: Vec<String>,
    demo: bool,
    generation: Arc<AtomicU64>,
}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            symbols: Vec::new(),
            demo: false,
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn set_symbols(&mut self, symbols: Vec<String>, demo: bool) {
        let demo = demo || SETTINGS.force_demo;
        if symbols == self.symbols && demo == self.demo {
            return;
        }
        self.symbols = symbols.clone();
        self.demo = demo;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        for task in self.tasks.drain(..) {
            task.abort();
        }
        {
            let mut state = STATE.lock();
            state.source = if demo { "DEMO".into() } else { "LIVE".into() };
            state.connections = 0;
            state.books.clear();
            state.trades.clear();
            state.positions.clear();
            state.metrics.clear();
            state.session += 1;
        }
        if demo {
            let gen_ref = self.generation.clone();
            self.tasks
                .push(tokio::spawn(demo_feed(symbols, generation, gen_ref)));
            STATE.lock().log(
                "DEMO",
                "Deterministic demo feed active; results reset",
                "SYSTEM",
                0.0,
            );
        } else {
            for chunk in symbols.chunks(SETTINGS.ws_chunk_size) {
                let gen_ref = self.generation.clone();
                self.tasks
                    .push(tokio::spawn(live_feed(chunk.to_vec(), generation, gen_ref)));
            }
        }
    }
}

async fn live_feed(symbols: Vec<String>, generation: u64, gen_ref: Arc<AtomicU64>) {
    let mut delay = 1.0_f64;
    let mut jitter = SmallRng::seed_from_u64(generation);
    while gen_ref.load(Ordering::SeqCst) == generation {
        match tokio_tungstenite::connect_async(&SETTINGS.ws_url).await {
            Ok((mut ws, _)) => {
                let mut args: Vec<String> = symbols
                    .iter()
                    .map(|s| format!("orderbook.50.{s}"))
                    .collect();
                args.extend(symbols.iter().map(|s| format!("publicTrade.{s}")));
                let subscribe = json!({"op": "subscribe", "args": args}).to_string();
                if ws.send(Message::Text(subscribe.into())).await.is_ok() {
                    STATE.lock().connections += 1;
                    delay = 1.0;
                    while gen_ref.load(Ordering::SeqCst) == generation {
                        match ws.next().await {
                            Some(Ok(Message::Text(raw))) => {
                                if let Ok(msg) = serde_json::from_str::<Value>(&raw) {
                                    handle_message(&msg);
                                } else {
                                    STATE.lock().log(
                                        "WARN",
                                        "Malformed stream payload",
                                        "SYSTEM",
                                        10.0,
                                    );
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                let _ = ws.send(Message::Pong(payload)).await;
                            }
                            Some(Ok(_)) => {}
                            _ => break,
                        }
                    }
                }
                let mut state = STATE.lock();
                state.connections = (state.connections - 1).max(0);
            }
            Err(exc) => {
                let message = format!("Stream reconnecting: {:.70}", exc.to_string());
                STATE.lock().log("WARN", &message, "SYSTEM", 8.0);
            }
        }
        tokio::time::sleep(Duration::from_secs_f64(delay + jitter.random::<f64>())).await;
        delay = (delay * 2.0).min(30.0);
    }
}

fn parse_levels(raw: Option<&Value>) -> Vec<(f64, f64)> {
    raw.and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let pair = row.as_array()?;
                    let price: f64 = pair.first()?.as_str()?.parse().ok()?;
                    let size: f64 = pair.get(1)?.as_str()?.parse().ok()?;
                    Some((price, size))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn handle_message(msg: &Value) {
    let topic = msg["topic"].as_str().unwrap_or("");
    let data = &msg["data"];
    if topic.starts_with("orderbook") && !data.is_null() {
        let Some(symbol) = data["s"].as_str() else {
            return;
        };
        let kind = msg["type"].as_str().unwrap_or("delta");
        let bids = parse_levels(data.get("b"));
        let asks = parse_levels(data.get("a"));
        let sequence = data["u"].as_i64().unwrap_or(0);
        let previous_update = data["pu"].as_i64();
        let exchange_ts = msg["ts"].as_f64().unwrap_or(0.0) / 1_000.0;
        let received_at = now_ts();
        let recorded = recorder::record("orderbook", symbol, exchange_ts, received_at, sequence, data);
        let mut state = STATE.lock();
        if !recorded {
            state.mark_data_gap(symbol, received_at);
            state.log("WARN", "Raw recorder channel overflow", symbol, 1.0);
        }
        let previous = state.books.get(symbol).map(|book| book.sequence).unwrap_or(0);
        if kind != "snapshot" && previous > 0 && previous_update.is_some_and(|pu| pu != previous) {
            state.mark_data_gap(symbol, received_at);
            state.log("WARN", &format!("Order book sequence gap: expected parent {previous}, got {previous_update:?}"), symbol, 1.0);
        }
        let book = state
            .books
            .entry(symbol.to_string())
            .or_insert_with(OrderBook::default);
        book.apply(kind, &bids, &asks, sequence);
        book.exchange_timestamp = exchange_ts;
        book.local_receive_timestamp = received_at;
    } else if topic.starts_with("publicTrade") {
        if let Some(rows) = data.as_array() {
            let mut state = STATE.lock();
            for t in rows {
                let (Some(symbol), Some(ts), Some(side)) =
                    (t["s"].as_str(), t["T"].as_f64(), t["S"].as_str())
                else {
                    continue;
                };
                let price: f64 = t["p"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let size: f64 = t["v"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let symbol = symbol.to_string();
                let local_ts = now_ts();
                if !recorder::record("trade", &symbol, ts / 1000.0, local_ts, 0, t) {
                    state.mark_data_gap(&symbol, local_ts);
                    state.log("WARN", "Raw recorder channel overflow", &symbol, 1.0);
                }
                state.push_tick(
                    &symbol,
                    TradeTick {
                        timestamp: ts / 1000.0,
                        is_buy: side == "Buy",
                        price,
                        size,
                    },
                );
            }
        }
    }
}

/// Deterministic demo generator, mirroring the Python simulator exactly in
/// structure: sinusoidal drift, biased walls at level 5, tape following bias.
async fn demo_feed(symbols: Vec<String>, generation: u64, gen_ref: Arc<AtomicU64>) {
    let mut seed = SmallRng::seed_from_u64(73_421);
    let base = |symbol: &str| -> f64 {
        match symbol {
            "BTCUSDT" => 66_500.0,
            "ETHUSDT" => 3_480.0,
            "SOLUSDT" => 148.0,
            "XRPUSDT" => 0.52,
            "DOGEUSDT" => 0.128,
            "LINKUSDT" => 14.7,
            "AVAXUSDT" => 31.2,
            "SUIUSDT" => 0.91,
            _ => 10.0,
        }
    };
    let mut ticks: u64 = 0;
    STATE.lock().connections = 1;
    while gen_ref.load(Ordering::SeqCst) == generation {
        ticks += 1;
        {
            let mut state = STATE.lock();
            for (idx, symbol) in symbols.iter().enumerate() {
                let t = ticks as f64;
                let i = idx as f64;
                let price = base(symbol)
                    * (1.0 + (t / 37.0 + i).sin() * 0.0018 + seed.random_range(-0.00035..0.00035));
                let bias = (t / 14.0 + i * 1.7).sin();
                let mut bids = Vec::with_capacity(50);
                let mut asks = Vec::with_capacity(50);
                for level in 1..=50_i32 {
                    let mut size =
                        (8.0 + seed.random::<f64>() * 14.0) * (1.0 + bias.max(0.0) * 1.7);
                    if level == 5 && bias > 0.45 {
                        size *= 7.0;
                    }
                    bids.push((price * (1.0 - level as f64 * 0.00006), size));
                    let mut size =
                        (8.0 + seed.random::<f64>() * 14.0) * (1.0 + (-bias).max(0.0) * 1.7);
                    if level == 5 && bias < -0.45 {
                        size *= 7.0;
                    }
                    asks.push((price * (1.0 + level as f64 * 0.00006), size));
                }
                let book = state
                    .books
                    .entry(symbol.clone())
                    .or_insert_with(OrderBook::default);
                book.apply("snapshot", &bids, &asks, ticks as i64);
                let is_buy = seed.random::<f64>() < 0.5 + bias * 0.28;
                let size = seed.random_range(2.0..45.0);
                state.push_tick(
                    symbol,
                    TradeTick {
                        timestamp: now_ts(),
                        is_buy,
                        price,
                        size,
                    },
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
