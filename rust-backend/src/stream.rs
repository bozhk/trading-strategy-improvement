use crate::config::SETTINGS;
use crate::models::{now_ts, TradeTick};
use crate::mtf_fvg::{Candle, Timeframe};
use crate::state::STATE;
use futures_util::{SinkExt, StreamExt};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use std::collections::HashSet;
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

    pub fn is_demo(&self) -> bool {
        self.demo || SETTINGS.force_demo
    }

    pub async fn set_symbols(&mut self, mut symbols: Vec<String>, demo: bool) {
        let demo = demo || SETTINGS.force_demo;
        symbols.sort();
        symbols.dedup();

        let mut effective_symbols = symbols;
        {
            let mut state = STATE.lock();
            if demo != self.demo && !self.symbols.is_empty() && !state.positions.is_empty() {
                state.log(
                    "WARN",
                    "Data-source switch deferred until all paper positions are closed",
                    "SYSTEM",
                    30.0,
                );
                return;
            }
            effective_symbols.extend(state.positions.keys().cloned());
        }
        effective_symbols.sort();
        effective_symbols.dedup();

        if effective_symbols == self.symbols && demo == self.demo {
            return;
        }
        let source_changed = demo != self.demo;
        self.symbols = effective_symbols.clone();
        self.demo = demo;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        for task in self.tasks.drain(..) {
            task.abort();
        }
        {
            let mut state = STATE.lock();
            state.source = if demo { "DEMO".into() } else { "LIVE".into() };
            state.connections = 0;
            if source_changed {
                state.books.clear();
                state.trades.clear();
                state.mtf_bars.clear();
                state.mtf_fvg.clear();
                state.metrics.clear();
            } else {
                let retained: HashSet<&str> =
                    effective_symbols.iter().map(String::as_str).collect();
                state
                    .books
                    .retain(|symbol, _| retained.contains(symbol.as_str()));
                state
                    .trades
                    .retain(|symbol, _| retained.contains(symbol.as_str()));
                state
                    .mtf_bars
                    .retain(|symbol, _| retained.contains(symbol.as_str()));
                state
                    .mtf_fvg
                    .retain(|symbol, _| retained.contains(symbol.as_str()));
                state
                    .metrics
                    .retain(|symbol, _| retained.contains(symbol.as_str()));
            }
            state.wall_tracks.clear();
            state.pending_signals.clear();
            state.mtf_fvg.clear();
            state.session += 1;
        }
        if demo {
            seed_demo_mtf_history(&effective_symbols);
            let gen_ref = self.generation.clone();
            self.tasks.push(tokio::spawn(demo_feed(
                effective_symbols,
                generation,
                gen_ref,
            )));
            STATE
                .lock()
                .log("DEMO", "Deterministic demo feed active", "SYSTEM", 0.0);
        } else {
            for chunk in effective_symbols.chunks(SETTINGS.ws_chunk_size) {
                let gen_ref = self.generation.clone();
                self.tasks
                    .push(tokio::spawn(live_feed(chunk.to_vec(), generation, gen_ref)));
            }
        }
    }
}

fn demo_base(symbol: &str) -> f64 {
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
}

fn demo_history(timeframe: Timeframe, base: f64, now: f64) -> Vec<Candle> {
    let duration = timeframe.seconds();
    let current_start = (now.floor() as i64).div_euclid(duration) * duration;
    let mut candles: Vec<Candle> = (0..100)
        .map(|index| {
            let started_at = current_start - (100 - index) as i64 * duration;
            let center = base * (1.0 + ((index as f64) / 9.0).sin() * 0.0008);
            Candle {
                timeframe,
                started_at,
                open: center * 0.9999,
                high: center * 1.0003,
                low: center * 0.9997,
                close: center * 1.0001,
                volume: 1_000.0,
                trades: 0,
            }
        })
        .collect();
    if timeframe == Timeframe::M15 {
        let len = candles.len();
        let first = &mut candles[len - 3];
        first.open = base * 0.9975;
        first.high = base * 0.9982;
        first.low = base * 0.9972;
        first.close = base * 0.9980;
        let middle = &mut candles[len - 2];
        middle.open = base * 0.9980;
        middle.high = base * 1.0002;
        middle.low = base * 0.9978;
        middle.close = base * 1.0000;
        let last = &mut candles[len - 1];
        last.open = base * 0.9990;
        last.high = base * 1.0000;
        last.low = base * 0.9988;
        last.close = base * 0.9997;
    }
    candles
}

fn seed_demo_mtf_history(symbols: &[String]) {
    if SETTINGS.strategy_mode != "fvg" {
        return;
    }
    let now = now_ts();
    let mut state = STATE.lock();
    for symbol in symbols {
        let base = demo_base(symbol);
        let bars = state.mtf_bars.entry(symbol.clone()).or_default();
        bars.seed(Timeframe::M15, demo_history(Timeframe::M15, base, now));
        bars.seed(Timeframe::M1, demo_history(Timeframe::M1, base, now));
        bars.mark_history_valid();
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
                for symbol in &symbols {
                    state.mtf_fvg.remove(symbol);
                    if let Some(bars) = state.mtf_bars.get_mut(symbol) {
                        bars.handle_disconnect(now_ts());
                    }
                }
                state.log(
                    "WARN",
                    "MTF FVG state reset after market stream disconnect",
                    "SYSTEM",
                    10.0,
                );
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
        let mut state = STATE.lock();
        let accepted = state
            .books
            .entry(symbol.to_string())
            .or_default()
            .apply(kind, &bids, &asks, sequence);
        if !accepted {
            state.data_quality_errors += 1;
        }
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
    let mut ticks: u64 = 0;
    STATE.lock().connections = 1;
    while gen_ref.load(Ordering::SeqCst) == generation {
        ticks += 1;
        {
            let mut state = STATE.lock();
            for (idx, symbol) in symbols.iter().enumerate() {
                let t = ticks as f64;
                let i = idx as f64;
                let price = demo_base(symbol)
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
                let book = state.books.entry(symbol.clone()).or_default();
                let _ = book.apply("snapshot", &bids, &asks, ticks as i64);
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
