use crate::models::now_ts;
use crate::state::MarketState;
use ordered_float::OrderedFloat;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn serialize_levels(levels: &BTreeMap<OrderedFloat<f64>, f64>, reverse: bool) -> Vec<Value> {
    let rows: Vec<(f64, f64)> = if reverse {
        levels
            .iter()
            .rev()
            .take(50)
            .map(|(p, q)| (p.0, *q))
            .collect()
    } else {
        levels.iter().take(50).map(|(p, q)| (p.0, *q)).collect()
    };
    rows.into_iter()
        .map(|(price, size)| {
            json!({
                "price": price, "size": size, "notional": price * size,
            })
        })
        .collect()
}

fn symbol_walls(state: &MarketState, symbol: &str, now: f64) -> Vec<Value> {
    let mut walls = Vec::new();
    for side in ["bid", "ask"] {
        let Some(wall) = state.wall_tracks.get(&(symbol.to_string(), side)) else {
            continue;
        };
        let age_seconds = (now - wall.started_at).max(0.0);
        let current_size = state
            .books
            .get(symbol)
            .and_then(|book| {
                let levels = if side == "bid" {
                    &book.bids
                } else {
                    &book.asks
                };
                levels.get(&OrderedFloat(wall.price)).copied()
            })
            .unwrap_or(0.0);
        walls.push(json!({
            "side": side.to_uppercase(),
            "price": wall.price,
            "initial_size": wall.initial_size,
            "current_size": current_size,
            "verified": wall.verified,
            "triggered": wall.triggered,
            "age_seconds": age_seconds,
            "confirmation_seconds": 3.0,
            "aging_progress": (age_seconds / 3.0).min(1.0),
        }));
    }
    walls
}

fn symbol_history(state: &MarketState, symbol: &str) -> Vec<Value> {
    let mut history: Vec<(f64, Value)> = Vec::new();
    for trade in state.closed.iter().filter(|t| t.symbol == symbol) {
        history.push((
            trade.closed_at,
            json!({
                "kind": "TRADE", "timestamp": trade.closed_at, "side": trade.side,
                "message": trade.reason, "pnl": trade.pnl,
                "entry": trade.entry, "exit": trade.exit,
            }),
        ));
    }
    for event in state.logs.iter().filter(|e| e.symbol == symbol) {
        history.push((
            event.timestamp,
            json!({
                "kind": "LOG", "timestamp": event.timestamp,
                "level": event.level, "message": event.message,
            }),
        ));
    }
    history.sort_by(|a, b| b.0.total_cmp(&a.0));
    history
        .into_iter()
        .take(5)
        .map(|(_, value)| value)
        .collect()
}

pub fn build_symbol_detail(state: &MarketState, symbol: &str) -> Option<Value> {
    let now = now_ts();
    let book = state.books.get(symbol)?;
    let (bid, ask) = book.quote();
    let spread_pct = if bid > 0.0 && ask > 0.0 {
        (ask - bid) / bid * 100.0
    } else {
        0.0
    };
    let metrics = state
        .metrics
        .get(symbol)
        .cloned()
        .unwrap_or_else(|| json!({}));

    let f = |key: &str, default: f64| metrics[key].as_f64().unwrap_or(default);
    let buy_accelerating = metrics["buy_accelerating"].as_bool().unwrap_or(false);
    let sell_accelerating = metrics["sell_accelerating"].as_bool().unwrap_or(false);

    Some(json!({
        "symbol": symbol,
        "timestamp": now,
        "book_updated_at": book.updated_at,
        "quote": {
            "bid": bid,
            "ask": ask,
            "mid": if bid > 0.0 && ask > 0.0 { (bid + ask) / 2.0 } else { 0.0 },
            "spread_pct": spread_pct,
        },
        "book": {
            "bids": serialize_levels(&book.bids, true),
            "asks": serialize_levels(&book.asks, false),
        },
        "brain": {
            "status": metrics["status"].as_str().unwrap_or("WATCH"),
            "walls": symbol_walls(state, symbol, now),
            "btc_trend": metrics["btc_trend"].as_str().unwrap_or("UNKNOWN"),
            "btc_trend_pct": f("btc_trend_pct", 0.0),
            "btc_trend_coverage": f("btc_trend_coverage", 0.0),
            "buy_accelerating": buy_accelerating,
            "sell_accelerating": sell_accelerating,
            "tape_accelerating": buy_accelerating || sell_accelerating,
            "buy_acceleration": f("buy_acceleration", 0.0),
            "sell_acceleration": f("sell_acceleration", 0.0),
            "buy_dominance": f("buy_dominance", 0.5),
            "sell_dominance": f("sell_dominance", 0.5),
            "flow": f("flow", 0.5),
            "imbalance": f("imbalance", 0.5),
        },
        "history": symbol_history(state, symbol),
    }))
}
