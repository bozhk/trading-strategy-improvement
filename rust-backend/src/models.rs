use ordered_float::OrderedFloat;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Side {
    LONG,
    SHORT,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Side::LONG => "LONG",
            Side::SHORT => "SHORT",
        }
    }
    pub fn direction(&self) -> f64 {
        match self {
            Side::LONG => 1.0,
            Side::SHORT => -1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Instrument {
    pub symbol: String,
    pub price: f64,
    pub turnover24h: f64,
    pub spread_pct: f64,
}

#[derive(Debug, Clone, Default)]
pub struct OrderBook {
    pub bids: BTreeMap<OrderedFloat<f64>, f64>,
    pub asks: BTreeMap<OrderedFloat<f64>, f64>,
    pub updated_at: f64,
    pub sequence: i64,
}

impl OrderBook {
    pub fn apply(&mut self, kind: &str, bids: &[(f64, f64)], asks: &[(f64, f64)], sequence: i64) {
        if kind == "snapshot" {
            self.bids.clear();
            self.asks.clear();
        }
        for (target, levels) in [(&mut self.bids, bids), (&mut self.asks, asks)] {
            for &(price, size) in levels {
                if size == 0.0 {
                    target.remove(&OrderedFloat(price));
                } else {
                    target.insert(OrderedFloat(price), size);
                }
            }
        }
        // Keep top 50 on each side, mirroring the Python book.
        while self.bids.len() > 50 {
            let lowest = *self.bids.keys().next().unwrap();
            self.bids.remove(&lowest);
        }
        while self.asks.len() > 50 {
            let highest = *self.asks.keys().next_back().unwrap();
            self.asks.remove(&highest);
        }
        self.sequence = self.sequence.max(sequence);
        self.updated_at = now_ts();
    }

    /// Best bid / best ask (0.0 when empty, mirroring the Python engine).
    pub fn quote(&self) -> (f64, f64) {
        let bid = self.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let ask = self.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        (bid, ask)
    }
}

#[derive(Debug, Clone)]
pub struct TradeTick {
    pub timestamp: f64,
    pub is_buy: bool,
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone)]
pub struct Position {
    pub symbol: String,
    pub side: Side,
    pub entry: f64,
    pub quantity: f64,
    pub opened_at: f64,
    pub mark: f64,
    pub stop_price: f64,
    pub target_price: f64,
    pub risk_per_unit: f64,
    pub entry_fee: f64,
    pub entry_slippage_cost: f64,
    pub signal_snapshot: Value,
    pub unrealized: f64,
    pub breakeven_armed: bool,
    pub peak_return: f64,
    pub trough_return: f64,
    pub mfe: f64,
    pub mae: f64,
    pub reversal_streak: u32,
    pub last_status_log: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClosedTrade {
    pub symbol: String,
    pub side: &'static str,
    pub entry: f64,
    pub exit: f64,
    pub pnl: f64,
    pub gross_pnl: f64,
    pub fees: f64,
    pub slippage_cost: f64,
    pub reason: String,
    pub opened_at: f64,
    pub closed_at: f64,
    pub mfe: f64,
    pub mae: f64,
    pub signal_snapshot: Value,
    pub exit_snapshot: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupAnalysis {
    pub id: u64,
    pub timestamp: f64,
    pub symbol: String,
    pub side: String,
    pub decision: String,
    pub reason: String,
    pub wall: f64,
    pub breakout_price: f64,
    pub retest_price: f64,
    pub entry: f64,
    pub stop: f64,
    pub target: f64,
    pub stop_pct: f64,
    pub target_pct: f64,
    pub gross_rr: f64,
    pub cost_pct: f64,
    pub cost_coverage: f64,
    pub stop_cost_ratio: f64,
    pub net_rr: f64,
    pub required_net_rr: f64,
    pub score: i64,
    pub score_required: i64,
    pub score_breakdown: Value,
    pub filter_actual: f64,
    pub filter_required: f64,
    pub filter_gap: f64,
    pub imbalance: f64,
    pub acceleration: f64,
    pub acceleration_current_notional: f64,
    pub acceleration_baseline_notional: f64,
    pub acceleration_baseline_valid: bool,
    pub spread: f64,
    pub freshness: f64,
    pub absorption_initial_size: f64,
    pub absorption_current_size: f64,
    pub absorption_depleted_quantity: f64,
    pub absorption_executed_quantity: f64,
    pub absorption_matched: f64,
    pub btc_trend: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VirtualOutcome {
    pub setup_id: u64,
    pub symbol: String,
    pub side: String,
    pub started_at: f64,
    pub expires_at: f64,
    pub entry: f64,
    pub stop: f64,
    pub target: f64,
    pub last_price: f64,
    pub mfe_pct: f64,
    pub mae_pct: f64,
    pub outcome: String,
    pub resolved_at: Option<f64>,
    pub price_30s: Option<f64>,
    pub price_1m: Option<f64>,
    pub price_3m: Option<f64>,
    pub price_5m: Option<f64>,
    pub price_15m: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEvent {
    pub level: String,
    pub message: String,
    pub symbol: String,
    pub timestamp: f64,
}

/// One active liquidity-wall observation per (symbol, side).
#[derive(Debug, Clone)]
pub struct WallTrack {
    #[allow(dead_code)] // diagnostic field, mirrors the Python engine
    pub side: &'static str,
    pub price: f64,
    pub initial_size: f64,
    pub peak_size: f64,
    pub started_at: f64,
    pub verified: bool,
    pub triggered: bool,
}

#[derive(Debug, Clone)]
pub struct Absorption {
    pub side: &'static str,
    pub price: f64,
    pub initial_size: f64,
    pub current_size: f64,
    pub depleted_quantity: f64,
    pub depletion_ratio: f64,
    pub executed_quantity: f64,
    pub matched_ratio: f64,
    pub verified_for: f64,
    pub absorbed: bool,
}

/// Qualified absorption waiting for breakout + held retest.
#[derive(Debug, Clone)]
pub struct PendingSignal {
    pub side: Side,
    #[allow(dead_code)] // recorded for parity with the Python engine
    pub wall: f64,
    pub created: f64,
    pub broken: bool,
    pub hold_ticks: u32,
    #[allow(dead_code)] // recorded for parity with the Python engine
    pub score: i64,
    pub absorption: Absorption,
}
