use crate::models::{
    now_ts, ClosedTrade, LogEvent, OrderBook, PendingSignal, Position, TradeTick, WallTrack,
};
use crate::mtf_fvg::{MtfBars, MtfFvgTracker};
use crate::readiness::live_readiness;
use crate::telegram;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;

pub const TRADES_MAXLEN: usize = 1200;
const CLOSED_MAXLEN: usize = 1000;
const LOGS_MAXLEN: usize = 180;
pub const BTC_HISTORY_MAXLEN: usize = 400;

#[derive(Default)]
pub struct MarketState {
    pub books: HashMap<String, OrderBook>,
    pub trades: HashMap<String, VecDeque<TradeTick>>,
    pub mtf_bars: HashMap<String, MtfBars>,
    pub mtf_fvg: HashMap<String, MtfFvgTracker>,
    pub positions: HashMap<String, Position>,
    pub closed: VecDeque<ClosedTrade>,
    pub logs: VecDeque<LogEvent>,
    pub radar: Vec<Value>,
    pub metrics: HashMap<String, Value>,
    pub cooldowns: HashMap<String, f64>,
    pub wall_tracks: HashMap<(String, &'static str), WallTrack>,
    pub pending_signals: HashMap<String, PendingSignal>,
    pub reject_counts: HashMap<String, u64>,
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
        self.mtf_bars
            .entry(symbol.to_string())
            .or_default()
            .update(&tick);
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
        telegram::notify_rejection(symbol, reason);
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
        exit_reasons.sort_by_key(|item| std::cmp::Reverse(item.1));

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
            "mtf_fvg": self.mtf_fvg.iter().map(|(symbol, tracker)| json!({
                "symbol": symbol,
                "state": tracker.snapshot(),
            })).collect::<Vec<_>>(),
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
