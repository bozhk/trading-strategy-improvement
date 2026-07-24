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
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Long,
    Short,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Long => "LONG",
            Side::Short => "SHORT",
        }
    }
    pub fn direction(&self) -> f64 {
        match self {
            Side::Long => 1.0,
            Side::Short => -1.0,
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
    pub fn apply(
        &mut self,
        kind: &str,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
        sequence: i64,
    ) -> bool {
        // A snapshot starts a new sequence. Deltas at or behind the current
        // sequence are stale and must not refresh or mutate the live book.
        if kind != "snapshot" && sequence <= self.sequence {
            return false;
        }
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
        self.sequence = sequence;
        self.updated_at = now_ts();
        true
    }

    /// Best bid / best ask (0.0 when empty, mirroring the Python engine).
    pub fn quote(&self) -> (f64, f64) {
        let bid = self.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let ask = self.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        (bid, ask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_delta_does_not_mutate_the_book() {
        let mut book = OrderBook::default();
        assert!(book.apply("snapshot", &[(100.0, 2.0)], &[(101.0, 3.0)], 10));
        assert!(!book.apply("delta", &[(100.0, 9.0)], &[], 9));
        assert_eq!(book.bids.get(&OrderedFloat(100.0)), Some(&2.0));
        assert_eq!(book.sequence, 10);
    }

    #[test]
    fn snapshot_can_start_a_new_sequence() {
        let mut book = OrderBook::default();
        assert!(book.apply("snapshot", &[(100.0, 2.0)], &[], 10));
        assert!(book.apply("snapshot", &[(99.0, 4.0)], &[], 1));
        assert!(!book.bids.contains_key(&OrderedFloat(100.0)));
        assert_eq!(book.bids.get(&OrderedFloat(99.0)), Some(&4.0));
        assert_eq!(book.sequence, 1);
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
    pub reversal_started_at: f64,
    pub last_status_log: f64,
}

#[derive(Debug, Clone)]
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
