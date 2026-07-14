use crate::models::{
    now_ts, ClosedTrade, LogEvent, OrderBook, PendingSignal, Position, TradeTick, WallTrack,
};
use crate::readiness::live_readiness;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;

pub const TRADES_MAXLEN: usize = 1200;
const CLOSED_MAXLEN: usize = 1000;
const LOGS_MAXLEN: usize = 180;
const DIAGNOSTICS_MAXLEN: usize = 20_000;
const DIAGNOSTICS_WINDOW_SECONDS: f64 = 3_600.0;
const BACKGROUND_DIAGNOSTIC_DEDUP_SECONDS: f64 = 10.0;
pub const BTC_HISTORY_MAXLEN: usize = 400;

#[derive(Debug, Clone)]
pub struct DiagnosticEvent {
    pub timestamp: f64,
    pub symbol: String,
    pub stage: String,
    pub reason: Option<String>,
    pub score: Option<i64>,
    pub imbalance: Option<f64>,
    pub acceleration: Option<f64>,
    pub spread: Option<f64>,
}

#[derive(Default)]
pub struct MarketState {
    pub books: HashMap<String, OrderBook>,
    pub trades: HashMap<String, VecDeque<TradeTick>>,
    pub positions: HashMap<String, Position>,
    pub closed: VecDeque<ClosedTrade>,
    pub logs: VecDeque<LogEvent>,
    pub radar: Vec<Value>,
    pub metrics: HashMap<String, Value>,
    pub cooldowns: HashMap<String, f64>,
    pub wall_tracks: HashMap<(String, &'static str), WallTrack>,
    pub pending_signals: HashMap<String, PendingSignal>,
    pub reject_counts: HashMap<String, u64>,
    pub diagnostics: VecDeque<DiagnosticEvent>,
    pub data_quality_errors: u64,
    pub btc_mid_history: VecDeque<(f64, f64)>,
    pub source: String,
    pub connections: i64,
    pub last_scan: f64,
    pub started_at: f64,
    pub session: u64,
    log_throttle: HashMap<String, f64>,
}

impl MarketState {
    fn new() -> Self {
        Self {
            source: "CONNECTING".into(),
            started_at: now_ts(),
            session: 1,
            ..Default::default()
        }
    }

    pub fn push_tick(&mut self, symbol: &str, tick: TradeTick) {
        let queue = self.trades.entry(symbol.to_string()).or_default();
        if queue.len() >= TRADES_MAXLEN {
            queue.pop_front();
        }
        queue.push_back(tick);
    }

    pub fn push_closed(&mut self, trade: ClosedTrade) {
        if self.closed.len() >= CLOSED_MAXLEN {
            self.closed.pop_back();
        }
        self.closed.push_front(trade);
    }

    pub fn log(&mut self, level: &str, message: &str, symbol: &str, throttle: f64) {
        let key = format!("{symbol}:{message}");
        let now = now_ts();
        if throttle > 0.0 && now - self.log_throttle.get(&key).copied().unwrap_or(0.0) < throttle {
            return;
        }
        self.log_throttle.insert(key, now);
        if self.logs.len() >= LOGS_MAXLEN {
            self.logs.pop_back();
        }
        self.logs.push_front(LogEvent {
            level: level.into(),
            message: message.into(),
            symbol: symbol.into(),
            timestamp: now,
        });
    }

    pub fn reject(&mut self, symbol: &str, reason: &str) {
        *self.reject_counts.entry(reason.to_string()).or_insert(0) += 1;
        self.log("SKIP", &format!("ENTRY REJECTED · {reason}"), symbol, 5.0);
    }

    pub fn diagnose(
        &mut self,
        symbol: &str,
        stage: &str,
        reason: Option<&str>,
        score: Option<i64>,
        imbalance: Option<f64>,
        acceleration: Option<f64>,
        spread: Option<f64>,
    ) {
        let now = now_ts();
        while self
            .diagnostics
            .front()
            .map(|event| now - event.timestamp > DIAGNOSTICS_WINDOW_SECONDS)
            .unwrap_or(false)
        {
            self.diagnostics.pop_front();
        }
        // Rejections without a score are background scanner checks, not setup
        // transitions. Sampling the same symbol/reason every loop filled an hour-long
        // buffer in minutes and made the funnel misleading.
        if stage == "rejected" && score.is_none() {
            let duplicate = self.diagnostics.iter().rev().any(|event| {
                now - event.timestamp <= BACKGROUND_DIAGNOSTIC_DEDUP_SECONDS
                    && event.symbol == symbol
                    && event.stage == stage
                    && event.reason.as_deref() == reason
            });
            if duplicate {
                return;
            }
        }
        if self.diagnostics.len() >= DIAGNOSTICS_MAXLEN {
            self.diagnostics.pop_front();
        }
        self.diagnostics.push_back(DiagnosticEvent {
            timestamp: now,
            symbol: symbol.to_string(),
            stage: stage.to_string(),
            reason: reason.map(str::to_string),
            score,
            imbalance,
            acceleration,
            spread,
        });
    }

    pub fn snapshot(&self) -> Value {
        let closed: Vec<&ClosedTrade> = self.closed.iter().collect();
        let pnls: Vec<f64> = closed.iter().map(|t| t.pnl).collect();
        let wins: Vec<f64> = pnls.iter().copied().filter(|v| *v > 0.0).collect();
        let losses: Vec<f64> = pnls.iter().copied().filter(|v| *v < 0.0).collect();
        let realized: f64 = pnls.iter().sum();
        let unrealized: f64 = self.positions.values().map(|p| p.unrealized).sum();

        let profit_factor: Value = if !losses.is_empty() {
            json!(wins.iter().sum::<f64>() / losses.iter().sum::<f64>().abs())
        } else if !wins.is_empty() {
            Value::Null
        } else {
            json!(0)
        };

        let mut leaderboard: HashMap<&str, f64> = HashMap::new();
        for trade in &closed {
            *leaderboard.entry(trade.symbol.as_str()).or_insert(0.0) += trade.pnl;
        }
        let mut leaderboard: Vec<(&str, f64)> = leaderboard.into_iter().collect();
        leaderboard.sort_by(|a, b| b.1.total_cmp(&a.1));
        leaderboard.truncate(8);

        // Equity curve (oldest -> newest) plus max drawdown.
        let mut chronological: Vec<&ClosedTrade> = closed.clone();
        chronological.sort_by(|a, b| a.closed_at.total_cmp(&b.closed_at));
        let mut equity_points = Vec::new();
        let (mut running, mut peak_equity, mut max_drawdown) = (0.0_f64, 0.0_f64, 0.0_f64);
        for trade in &chronological {
            running += trade.pnl;
            peak_equity = peak_equity.max(running);
            max_drawdown = max_drawdown.max(peak_equity - running);
            equity_points.push(json!({
                "t": trade.closed_at,
                "v": (running * 10_000.0).round() / 10_000.0,
            }));
        }
        let skip = equity_points.len().saturating_sub(150);
        let equity_points: Vec<Value> = equity_points.into_iter().skip(skip).collect();

        // Win/loss streak from most recent trades.
        let mut streak: i64 = 0;
        for trade in &closed {
            if trade.pnl == 0.0 {
                continue;
            }
            let sign: i64 = if trade.pnl > 0.0 { 1 } else { -1 };
            if streak == 0 {
                streak = sign;
            } else if (streak > 0) == (sign > 0) {
                streak += sign;
            } else {
                break;
            }
        }

        let hold_times: Vec<f64> = closed.iter().map(|t| t.closed_at - t.opened_at).collect();
        let mut exit_reasons: HashMap<&str, u64> = HashMap::new();
        for trade in &closed {
            *exit_reasons.entry(trade.reason.as_str()).or_insert(0) += 1;
        }
        let mut exit_reasons: Vec<(&str, u64)> = exit_reasons.into_iter().collect();
        exit_reasons.sort_by(|a, b| b.1.cmp(&a.1));

        let cutoff = now_ts() - DIAGNOSTICS_WINDOW_SECONDS;
        let diagnostics: Vec<&DiagnosticEvent> = self
            .diagnostics
            .iter()
            .filter(|event| event.timestamp >= cutoff)
            .collect();
        let mut stages: HashMap<&str, u64> = HashMap::new();
        let mut rejection_reasons: HashMap<&str, u64> = HashMap::new();
        let mut score_sum = 0_i64;
        let mut score_count = 0_u64;
        let mut imbalance_sum = 0.0;
        let mut imbalance_count = 0_u64;
        let mut acceleration_sum = 0.0;
        let mut acceleration_count = 0_u64;
        let mut spread_sum = 0.0;
        let mut spread_count = 0_u64;
        for event in &diagnostics {
            *stages.entry(event.stage.as_str()).or_insert(0) += 1;
            if let Some(reason) = event.reason.as_deref() {
                *rejection_reasons.entry(reason).or_insert(0) += 1;
            }
            if let Some(score) = event.score {
                score_sum += score;
                score_count += 1;
            }
            if let Some(value) = event.imbalance {
                imbalance_sum += value;
                imbalance_count += 1;
            }
            if let Some(value) = event.acceleration {
                acceleration_sum += value.min(100.0);
                acceleration_count += 1;
            }
            if let Some(value) = event.spread {
                spread_sum += value;
                spread_count += 1;
            }
        }
        let mut rejection_reasons: Vec<(&str, u64)> = rejection_reasons.into_iter().collect();
        rejection_reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let mut near_misses: Vec<&DiagnosticEvent> = diagnostics
            .iter()
            .copied()
            .filter(|event| event.stage == "rejected" || event.stage == "retest")
            .collect();
        near_misses.sort_by(|a, b| {
            b.score
                .unwrap_or_default()
                .cmp(&a.score.unwrap_or_default())
                .then_with(|| b.timestamp.total_cmp(&a.timestamp))
        });
        near_misses.truncate(8);

        json!({
            "version": 3,
            "timestamp": now_ts(),
            "live_readiness": live_readiness(&pnls, self.data_quality_errors),
            "reject_counts": self.reject_counts,
            "source": self.source,
            "connections": self.connections,
            "last_scan": self.last_scan,
            "uptime": now_ts() - self.started_at,
            "session": self.session,
            "mode": crate::config::SETTINGS.trading_mode.clone(),
            "stats": {
                "pnl": realized + unrealized,
                "realized": realized,
                "unrealized": unrealized,
                "win_rate": if closed.is_empty() { 0.0 } else { wins.len() as f64 / closed.len() as f64 * 100.0 },
                "profit_factor": profit_factor,
                "trades": closed.len(),
                "open": self.positions.len(),
                "wins": wins.len(),
                "losses": losses.len(),
                "avg_win": if wins.is_empty() { 0.0 } else { wins.iter().sum::<f64>() / wins.len() as f64 },
                "avg_loss": if losses.is_empty() { 0.0 } else { losses.iter().sum::<f64>() / losses.len() as f64 },
                "best_trade": if pnls.is_empty() { 0.0 } else { pnls.iter().copied().fold(f64::NEG_INFINITY, f64::max) },
                "worst_trade": if pnls.is_empty() { 0.0 } else { pnls.iter().copied().fold(f64::INFINITY, f64::min) },
                "max_drawdown": max_drawdown,
                "streak": streak,
                "avg_hold_seconds": if hold_times.is_empty() { 0.0 } else { hold_times.iter().sum::<f64>() / hold_times.len() as f64 },
            },
            "equity_curve": equity_points,
            "exit_reasons": exit_reasons.iter().map(|(reason, count)| json!({
                "reason": reason, "count": count,
            })).collect::<Vec<_>>(),
            "signal_diagnostics": {
                "window_seconds": DIAGNOSTICS_WINDOW_SECONDS,
                "events": diagnostics.len(),
                "stages": stages,
                "rejections": rejection_reasons.iter().take(8).map(|(reason, count)| json!({
                    "reason": reason, "count": count,
                })).collect::<Vec<_>>(),
                "averages": {
                    "score": if score_count == 0 { Value::Null } else { json!(score_sum as f64 / score_count as f64) },
                    "imbalance": if imbalance_count == 0 { Value::Null } else { json!(imbalance_sum / imbalance_count as f64) },
                    "acceleration": if acceleration_count == 0 { Value::Null } else { json!(acceleration_sum / acceleration_count as f64) },
                    "spread": if spread_count == 0 { Value::Null } else { json!(spread_sum / spread_count as f64) },
                },
                "near_misses": near_misses.iter().map(|event| json!({
                    "timestamp": event.timestamp, "symbol": event.symbol,
                    "stage": event.stage, "reason": event.reason,
                    "score": event.score, "imbalance": event.imbalance,
                    "acceleration": event.acceleration, "spread": event.spread,
                })).collect::<Vec<_>>(),
            },
            "closed_trades": closed.iter().take(40).map(|t| json!({
                "symbol": t.symbol, "side": t.side, "entry": t.entry, "exit": t.exit,
                "pnl": t.pnl, "reason": t.reason, "opened_at": t.opened_at,
                "closed_at": t.closed_at, "held": t.closed_at - t.opened_at,
                "gross_pnl": t.gross_pnl, "fees": t.fees,
                "slippage_cost": t.slippage_cost, "mfe": t.mfe, "mae": t.mae,
                "signal_snapshot": t.signal_snapshot,
                "exit_snapshot": t.exit_snapshot,
            })).collect::<Vec<_>>(),
            "radar": self.metrics.values().collect::<Vec<_>>(),
            "positions": self.positions.values().map(|p| json!({
                "symbol": p.symbol, "side": p.side.as_str(), "entry": p.entry,
                "quantity": p.quantity, "opened_at": p.opened_at, "mark": p.mark,
                "stop_price": p.stop_price, "target_price": p.target_price,
                "risk_per_unit": p.risk_per_unit, "entry_fee": p.entry_fee,
                "signal_snapshot": p.signal_snapshot, "unrealized": p.unrealized,
                "breakeven_armed": p.breakeven_armed, "peak_return": p.peak_return,
                "trough_return": p.trough_return, "mfe": p.mfe, "mae": p.mae,
                "reversal_streak": p.reversal_streak, "last_status_log": p.last_status_log,
            })).collect::<Vec<_>>(),
            "leaderboard": leaderboard.iter().map(|(symbol, pnl)| json!({
                "symbol": symbol, "pnl": pnl,
            })).collect::<Vec<_>>(),
            "logs": self.logs.iter().take(80).collect::<Vec<_>>(),
        })
    }
}

pub static STATE: LazyLock<Mutex<MarketState>> = LazyLock::new(|| Mutex::new(MarketState::new()));

#[cfg(test)]
mod tests {
    use super::{DiagnosticEvent, MarketState, DIAGNOSTICS_MAXLEN, DIAGNOSTICS_WINDOW_SECONDS};
    use crate::models::now_ts;

    #[test]
    fn diagnostics_are_bounded_and_prune_expired_events() {
        let mut state = MarketState::new();
        state.diagnostics.push_back(DiagnosticEvent {
            timestamp: now_ts() - DIAGNOSTICS_WINDOW_SECONDS - 1.0,
            symbol: "OLDUSDT".into(),
            stage: "rejected".into(),
            reason: Some("expired".into()),
            score: None,
            imbalance: None,
            acceleration: None,
            spread: None,
        });
        for _ in 0..=DIAGNOSTICS_MAXLEN {
            state.diagnose("BTCUSDT", "attempt", None, Some(80), None, None, None);
        }
        assert_eq!(state.diagnostics.len(), DIAGNOSTICS_MAXLEN);
        assert!(state
            .diagnostics
            .iter()
            .all(|event| event.symbol != "OLDUSDT"));
    }

    #[test]
    fn repeated_background_rejections_are_deduplicated() {
        let mut state = MarketState::new();
        for _ in 0..100 {
            state.diagnose(
                "BTCUSDT",
                "rejected",
                Some("low imbalance"),
                None,
                Some(0.5),
                None,
                Some(0.01),
            );
        }
        assert_eq!(state.diagnostics.len(), 1);
    }

    #[test]
    fn snapshot_aggregates_signal_diagnostics() {
        let mut state = MarketState::new();
        state.diagnose(
            "BTCUSDT",
            "rejected",
            Some("low imbalance"),
            Some(70),
            Some(0.5),
            Some(2.0),
            Some(0.01),
        );
        let snapshot = state.snapshot();
        assert_eq!(snapshot["signal_diagnostics"]["events"], 1);
        assert_eq!(snapshot["signal_diagnostics"]["stages"]["rejected"], 1);
        assert_eq!(
            snapshot["signal_diagnostics"]["rejections"][0]["reason"],
            "low imbalance"
        );
    }
}
