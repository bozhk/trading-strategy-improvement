use crate::execution::{book_fill, trade_pnl};
use crate::models::{
    now_ts, ClosedTrade, LogEvent, OrderBook, PendingSignal, Position, SetupAnalysis, TradeTick,
    VirtualOutcome, WallTrack,
};
use crate::config::SETTINGS;
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
const SETUP_ANALYSES_MAXLEN: usize = 2_000;
const COUNTERFACTUAL_MAXLEN: usize = SETUP_ANALYSES_MAXLEN * 9;
const VIRTUAL_TRACK_SECONDS: f64 = 900.0;
const TARGET_MULTIPLES: [f64; 3] = [1.8, 2.5, 3.0];
const LATENCY_SCENARIOS_MS: [u64; 3] = [50, 100, 200];
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

#[derive(Debug, Clone, Default)]
pub struct DiagnosticAggregate {
    pub events: u64,
    pub first_at: Option<f64>,
    pub last_at: Option<f64>,
    pub stages: HashMap<String, u64>,
    pub rejections: HashMap<String, u64>,
    pub stages_by_symbol: HashMap<String, HashMap<String, u64>>,
    pub rejections_by_symbol: HashMap<String, HashMap<String, u64>>,
    pub score_sum: f64,
    pub score_count: u64,
    pub imbalance_sum: f64,
    pub imbalance_count: u64,
    pub acceleration_sum: f64,
    pub acceleration_count: u64,
    pub spread_sum: f64,
    pub spread_count: u64,
}

impl DiagnosticAggregate {
    fn record(&mut self, event: &DiagnosticEvent) {
        self.events = self.events.saturating_add(1);
        self.first_at.get_or_insert(event.timestamp);
        self.last_at = Some(event.timestamp);
        *self.stages.entry(event.stage.clone()).or_insert(0) += 1;
        *self
            .stages_by_symbol
            .entry(event.symbol.clone())
            .or_default()
            .entry(event.stage.clone())
            .or_insert(0) += 1;
        if let Some(reason) = event.reason.as_deref() {
            *self.rejections.entry(reason.to_string()).or_insert(0) += 1;
            *self
                .rejections_by_symbol
                .entry(event.symbol.clone())
                .or_default()
                .entry(reason.to_string())
                .or_insert(0) += 1;
        }
        if let Some(value) = event.score {
            self.score_sum += value as f64;
            self.score_count += 1;
        }
        if let Some(value) = event.imbalance.filter(|value| value.is_finite()) {
            self.imbalance_sum += value;
            self.imbalance_count += 1;
        }
        if let Some(value) = event.acceleration.filter(|value| value.is_finite()) {
            self.acceleration_sum += value.min(100.0);
            self.acceleration_count += 1;
        }
        if let Some(value) = event.spread.filter(|value| value.is_finite()) {
            self.spread_sum += value;
            self.spread_count += 1;
        }
    }

    fn to_json(&self) -> Value {
        let average = |sum: f64, count: u64| {
            if count == 0 {
                Value::Null
            } else {
                json!(sum / count as f64)
            }
        };
        json!({
            "events": self.events,
            "first_at": self.first_at,
            "last_at": self.last_at,
            "stages": self.stages,
            "rejections": self.rejections,
            "stages_by_symbol": self.stages_by_symbol,
            "rejections_by_symbol": self.rejections_by_symbol,
            "averages": {
                "score": average(self.score_sum, self.score_count),
                "imbalance": average(self.imbalance_sum, self.imbalance_count),
                "acceleration": average(self.acceleration_sum, self.acceleration_count),
                "spread": average(self.spread_sum, self.spread_count),
            },
            "sample_counts": {
                "score": self.score_count,
                "imbalance": self.imbalance_count,
                "acceleration": self.acceleration_count,
                "spread": self.spread_count,
            },
        })
    }
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
    pub diagnostic_session: DiagnosticAggregate,
    pub diagnostics_dropped_capacity: u64,
    pub diagnostics_pruned_window: u64,
    pub setup_analyses: VecDeque<SetupAnalysis>,
    pub virtual_outcomes: VecDeque<VirtualOutcome>,
    pub setups_dropped_capacity: u64,
    pub outcomes_dropped_capacity: u64,
    pub next_setup_id: u64,
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

    pub fn mark_data_gap(&mut self, symbol: &str, now: f64) {
        self.data_quality_errors = self.data_quality_errors.saturating_add(1);
        for item in self.virtual_outcomes.iter_mut().filter(|item| {
            item.symbol == symbol
                && matches!(item.outcome.as_str(), "WAITING_FOR_FILL" | "TRACKING")
        }) {
            item.outcome = "DATA_GAP".into();
            item.resolved_at = Some(now);
        }
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
            self.diagnostics_pruned_window = self.diagnostics_pruned_window.saturating_add(1);
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
        let event = DiagnosticEvent {
            timestamp: now,
            symbol: symbol.to_string(),
            stage: stage.to_string(),
            reason: reason.map(str::to_string),
            score,
            imbalance,
            acceleration,
            spread,
        };
        self.diagnostic_session.record(&event);
        if self.diagnostics.len() >= DIAGNOSTICS_MAXLEN {
            self.diagnostics.pop_front();
            self.diagnostics_dropped_capacity =
                self.diagnostics_dropped_capacity.saturating_add(1);
        }
        self.diagnostics.push_back(event);
    }

    pub fn record_setup(&mut self, mut setup: SetupAnalysis, _track_virtual: bool) {
        self.next_setup_id = self.next_setup_id.saturating_add(1);
        setup.id = self.next_setup_id;
        let risk_per_unit = (setup.entry - setup.stop).abs();
        let risk_budget = SETTINGS.account_equity * SETTINGS.risk_per_trade_pct;
        let risk_quantity = if risk_per_unit > 0.0 { risk_budget / risk_per_unit } else { 0.0 };
        let notional_quantity = SETTINGS.max_position_notional / setup.entry.max(f64::MIN_POSITIVE);
        let quantity = risk_quantity.min(notional_quantity).max(0.0);
        for target_r_multiple in TARGET_MULTIPLES {
            for latency_ms in LATENCY_SCENARIOS_MS {
                self.virtual_outcomes.push_front(VirtualOutcome {
                    setup_id: setup.id,
                    variant_id: format!("taker_{target_r_multiple:.1}r_{latency_ms}ms"),
                    symbol: setup.symbol.clone(),
                    side: setup.side.clone(),
                    started_at: setup.timestamp,
                    fill_after: setup.timestamp + latency_ms as f64 / 1_000.0,
                    expires_at: setup.timestamp + VIRTUAL_TRACK_SECONDS,
                    target_r_multiple,
                    latency_ms,
                    requested_entry: setup.entry,
                    entry: None,
                    stop: setup.stop,
                    target: None,
                    quantity,
                    entry_fee: 0.0,
                    exit_fee: 0.0,
                    entry_slippage_cost: 0.0,
                    exit_slippage_cost: 0.0,
                    last_price: None,
                    exit_price: None,
                    gross_pnl: None,
                    net_pnl: None,
                    net_r: None,
                    mfe_pct: 0.0,
                    mae_pct: 0.0,
                    outcome: "WAITING_FOR_FILL".into(),
                    filled_at: None,
                    resolved_at: None,
                    price_30s: None,
                    price_1m: None,
                    price_3m: None,
                    price_5m: None,
                    price_15m: None,
                });
            }
        }
        if self.setup_analyses.len() >= SETUP_ANALYSES_MAXLEN {
            self.setup_analyses.pop_back();
            self.setups_dropped_capacity = self.setups_dropped_capacity.saturating_add(1);
        }
        self.setup_analyses.push_front(setup);
        while self.virtual_outcomes.len() > COUNTERFACTUAL_MAXLEN {
            self.virtual_outcomes.pop_back();
            self.outcomes_dropped_capacity = self.outcomes_dropped_capacity.saturating_add(1);
        }
    }

    pub fn update_virtual_outcomes(&mut self, symbol: &str, bid: f64, ask: f64, now: f64) {
        if !bid.is_finite() || !ask.is_finite() || bid <= 0.0 || ask <= 0.0 {
            return;
        }
        let Some(book) = self.books.get(symbol).cloned() else { return; };
        for outcome in self.virtual_outcomes.iter_mut().filter(|item| {
            item.symbol == symbol
                && matches!(item.outcome.as_str(), "WAITING_FOR_FILL" | "TRACKING")
        }) {
            let side = if outcome.side == "LONG" { crate::models::Side::LONG } else { crate::models::Side::SHORT };
            if outcome.outcome == "WAITING_FOR_FILL" {
                if now < outcome.fill_after { continue; }
                let Some(fill) = book_fill(&book, side, outcome.quantity, true) else {
                    outcome.outcome = "MISSED_AFTER_LATENCY".into();
                    outcome.resolved_at = Some(now);
                    continue;
                };
                let risk = (fill.price - outcome.stop).abs();
                if risk <= 0.0 || !risk.is_finite() {
                    outcome.outcome = "MISSED_AFTER_LATENCY".into();
                    outcome.resolved_at = Some(now);
                    continue;
                }
                outcome.entry = Some(fill.price);
                outcome.target = Some(fill.price + risk * outcome.target_r_multiple * side.direction());
                outcome.entry_fee = fill.fee;
                outcome.entry_slippage_cost = fill.slippage_cost;
                outcome.last_price = Some(fill.price);
                outcome.filled_at = Some(now);
                outcome.outcome = "TRACKING".into();
            }
            let Some(entry) = outcome.entry else { continue; };
            let Some(target) = outcome.target else { continue; };
            let price = if outcome.side == "LONG" { bid } else { ask };
            outcome.last_price = Some(price);
            let elapsed = now - outcome.started_at;
            let signed_return = (price - entry) / entry * side.direction() * 100.0;
            outcome.mfe_pct = outcome.mfe_pct.max(signed_return);
            outcome.mae_pct = outcome.mae_pct.min(signed_return);
            if elapsed >= 30.0 && outcome.price_30s.is_none() { outcome.price_30s = Some(price); }
            if elapsed >= 60.0 && outcome.price_1m.is_none() { outcome.price_1m = Some(price); }
            if elapsed >= 180.0 && outcome.price_3m.is_none() { outcome.price_3m = Some(price); }
            if elapsed >= 300.0 && outcome.price_5m.is_none() { outcome.price_5m = Some(price); }
            let target_hit = if outcome.side == "LONG" { price >= target } else { price <= target };
            let stop_hit = if outcome.side == "LONG" { price <= outcome.stop } else { price >= outcome.stop };
            let status = if stop_hit { Some("STOP_FIRST") } else if target_hit { Some("TARGET_FIRST") } else if now >= outcome.expires_at { Some("TIME_EXIT") } else { None };
            let Some(status) = status else { continue; };
            let Some(exit) = book_fill(&book, side, outcome.quantity, false) else {
                outcome.outcome = "DATA_GAP".into();
                outcome.resolved_at = Some(now);
                continue;
            };
            let (gross, net) = trade_pnl(side, entry, exit.price, outcome.quantity, outcome.entry_fee, exit.fee);
            let risk_cash = (entry - outcome.stop).abs() * outcome.quantity + outcome.entry_fee + exit.fee;
            outcome.exit_price = Some(exit.price);
            outcome.exit_fee = exit.fee;
            outcome.exit_slippage_cost = exit.slippage_cost;
            outcome.gross_pnl = Some(gross);
            outcome.net_pnl = Some(net);
            outcome.net_r = (risk_cash > 0.0).then_some(net / risk_cash);
            outcome.price_15m = (status == "TIME_EXIT").then_some(exit.price);
            outcome.outcome = status.into();
            outcome.resolved_at = Some(now);
        }
    }

    fn counterfactual_summary(&self) -> Value {
        #[derive(Default)]
        struct Aggregate { total: u64, filled: u64, wins: u64, losses: u64, time_exits: u64, data_gaps: u64, net_r_sum: f64, net_r_count: u64, mfe_sum: f64, mae_sum: f64, hold_sum: f64 }
        let mut groups: HashMap<String, Aggregate> = HashMap::new();
        for item in &self.virtual_outcomes {
            let group = groups.entry(item.variant_id.clone()).or_default();
            group.total += 1;
            if item.filled_at.is_some() { group.filled += 1; }
            group.wins += u64::from(item.outcome == "TARGET_FIRST");
            group.losses += u64::from(item.outcome == "STOP_FIRST");
            group.time_exits += u64::from(item.outcome == "TIME_EXIT");
            group.data_gaps += u64::from(item.outcome == "DATA_GAP");
            if let Some(value) = item.net_r { group.net_r_sum += value; group.net_r_count += 1; }
            group.mfe_sum += item.mfe_pct;
            group.mae_sum += item.mae_pct;
            if let (Some(filled), Some(resolved)) = (item.filled_at, item.resolved_at) { group.hold_sum += resolved - filled; }
        }
        let mut rows: Vec<Value> = groups.into_iter().map(|(variant_id, group)| {
            let resolved = group.wins + group.losses + group.time_exits;
            json!({
                "variant_id": variant_id,
                "sample_size": group.total,
                "fills": group.filled,
                "fill_rate": if group.total == 0 { 0.0 } else { group.filled as f64 / group.total as f64 },
                "wins": group.wins, "losses": group.losses, "time_exits": group.time_exits, "data_gaps": group.data_gaps,
                "win_rate": if resolved == 0 { Value::Null } else { json!(group.wins as f64 / resolved as f64) },
                "average_net_r": if group.net_r_count == 0 { Value::Null } else { json!(group.net_r_sum / group.net_r_count as f64) },
                "total_net_r": group.net_r_sum,
                "average_mfe_pct": if group.filled == 0 { Value::Null } else { json!(group.mfe_sum / group.filled as f64) },
                "average_mae_pct": if group.filled == 0 { Value::Null } else { json!(group.mae_sum / group.filled as f64) },
                "average_hold_seconds": if resolved == 0 { Value::Null } else { json!(group.hold_sum / resolved as f64) },
            })
        }).collect();
        rows.sort_by(|a, b| a["variant_id"].as_str().cmp(&b["variant_id"].as_str()));
        json!(rows)
    }

    fn strategy_summary(&self) -> Value {
        let mut decisions: HashMap<&str, u64> = HashMap::new();
        let mut reasons: HashMap<&str, u64> = HashMap::new();
        let mut setup_sides: HashMap<&str, u64> = HashMap::new();
        let mut setup_symbols: HashMap<&str, u64> = HashMap::new();
        let mut baseline_valid = 0_u64;
        let mut acceleration_points = 0_u64;
        let mut net_rr_sum = 0.0;
        let mut cost_coverage_sum = 0.0;
        let mut stop_cost_ratio_sum = 0.0;
        for setup in &self.setup_analyses {
            *decisions.entry(setup.decision.as_str()).or_insert(0) += 1;
            *reasons.entry(setup.reason.as_str()).or_insert(0) += 1;
            *setup_sides.entry(setup.side.as_str()).or_insert(0) += 1;
            *setup_symbols.entry(setup.symbol.as_str()).or_insert(0) += 1;
            baseline_valid += u64::from(setup.acceleration_baseline_valid);
            acceleration_points += u64::from(
                setup.score_breakdown["tape_acceleration"]
                    .as_i64()
                    .unwrap_or_default()
                    > 0,
            );
            net_rr_sum += setup.net_rr;
            cost_coverage_sum += setup.cost_coverage;
            stop_cost_ratio_sum += setup.stop_cost_ratio;
        }

        let mut outcomes: HashMap<&str, u64> = HashMap::new();
        let mut outcome_sides: HashMap<&str, HashMap<&str, u64>> = HashMap::new();
        let mut outcome_symbols: HashMap<&str, HashMap<&str, u64>> = HashMap::new();
        let mut mfe_sum = 0.0;
        let mut mae_sum = 0.0;
        for outcome in &self.virtual_outcomes {
            *outcomes.entry(outcome.outcome.as_str()).or_insert(0) += 1;
            *outcome_sides
                .entry(outcome.side.as_str())
                .or_default()
                .entry(outcome.outcome.as_str())
                .or_insert(0) += 1;
            *outcome_symbols
                .entry(outcome.symbol.as_str())
                .or_default()
                .entry(outcome.outcome.as_str())
                .or_insert(0) += 1;
            mfe_sum += outcome.mfe_pct;
            mae_sum += outcome.mae_pct;
        }
        let setup_count = self.setup_analyses.len() as f64;
        let outcome_count = self.virtual_outcomes.len() as f64;
        json!({
            "setups": {
                "total_available": self.setup_analyses.len(),
                "total_recorded": self.next_setup_id,
                "decisions": decisions,
                "reasons": reasons,
                "sides": setup_sides,
                "symbols": setup_symbols,
                "baseline_valid": baseline_valid,
                "baseline_invalid": self.setup_analyses.len() as u64 - baseline_valid,
                "with_acceleration_points": acceleration_points,
                "averages": {
                    "net_rr": if setup_count == 0.0 { Value::Null } else { json!(net_rr_sum / setup_count) },
                    "cost_coverage": if setup_count == 0.0 { Value::Null } else { json!(cost_coverage_sum / setup_count) },
                    "stop_cost_ratio": if setup_count == 0.0 { Value::Null } else { json!(stop_cost_ratio_sum / setup_count) },
                },
            },
            "virtual_outcomes": {
                "total_available": self.virtual_outcomes.len(),
                "outcomes": outcomes,
                "by_side": outcome_sides,
                "by_symbol": outcome_symbols,
                "averages": {
                    "mfe_pct": if outcome_count == 0.0 { Value::Null } else { json!(mfe_sum / outcome_count) },
                    "mae_pct": if outcome_count == 0.0 { Value::Null } else { json!(mae_sum / outcome_count) },
                },
            },
        })
    }

    pub fn diagnostics_export(&self) -> Value {
        let generated_at = now_ts();
        let snapshot = self.snapshot();
        let funnel_recent = snapshot["signal_diagnostics"].clone();
        let setup_timestamps: Vec<f64> = self
            .setup_analyses
            .iter()
            .map(|setup| setup.timestamp)
            .collect();
        let outcome_timestamps: Vec<f64> = self
            .virtual_outcomes
            .iter()
            .map(|outcome| outcome.started_at)
            .collect();
        json!({
            "version": 6,
            "generated_at": generated_at,
            "window_note": "Exact session aggregates cover accepted diagnostic events since engine start. Raw funnel events are a bounded recent one-hour window; setups are bounded to 2000 and counterfactual variants to 18000 items.",
            "metadata": {
                "session": self.session,
                "started_at": self.started_at,
                "generated_at": generated_at,
                "uptime_seconds": generated_at - self.started_at,
                "diagnostics": {
                    "recent_window_seconds": DIAGNOSTICS_WINDOW_SECONDS,
                    "capacity": DIAGNOSTICS_MAXLEN,
                    "current_length": self.diagnostics.len(),
                    "oldest_timestamp": self.diagnostics.front().map(|event| event.timestamp),
                    "newest_timestamp": self.diagnostics.back().map(|event| event.timestamp),
                    "dropped_by_capacity": self.diagnostics_dropped_capacity,
                    "pruned_by_window": self.diagnostics_pruned_window,
                    "truncated": self.diagnostics_dropped_capacity > 0 || self.diagnostics_pruned_window > 0,
                },
                "setups": {
                    "capacity": SETUP_ANALYSES_MAXLEN,
                    "current_length": self.setup_analyses.len(),
                    "total_recorded": self.next_setup_id,
                    "dropped_by_capacity": self.setups_dropped_capacity,
                    "oldest_timestamp": setup_timestamps.iter().copied().reduce(f64::min),
                    "newest_timestamp": setup_timestamps.iter().copied().reduce(f64::max),
                    "truncated": self.setups_dropped_capacity > 0,
                },
                "virtual_outcomes": {
                    "capacity": COUNTERFACTUAL_MAXLEN,
                    "current_length": self.virtual_outcomes.len(),
                    "dropped_by_capacity": self.outcomes_dropped_capacity,
                    "oldest_timestamp": outcome_timestamps.iter().copied().reduce(f64::min),
                    "newest_timestamp": outcome_timestamps.iter().copied().reduce(f64::max),
                    "truncated": self.outcomes_dropped_capacity > 0,
                },
            },
            "settings": crate::config::SETTINGS.clone(),
            "funnel": funnel_recent,
            "funnel_recent": snapshot["signal_diagnostics"].clone(),
            "funnel_session": self.diagnostic_session.to_json(),
            "strategy_summary": self.strategy_summary(),
            "setups": self.setup_analyses,
            "paper_trades": self.closed,
            "counterfactual_summary": self.counterfactual_summary(),
            "counterfactual_variants": self.virtual_outcomes,
            "virtual_outcomes": self.virtual_outcomes,
            "execution_model": "visible_l2_taker_vwap_v1",
        })
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
            "version": 6,
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
            "signal_diagnostics_session": self.diagnostic_session.to_json(),
            "signal_diagnostics": {
                "window_seconds": DIAGNOSTICS_WINDOW_SECONDS,
                "capacity": DIAGNOSTICS_MAXLEN,
                "dropped_by_capacity": self.diagnostics_dropped_capacity,
                "pruned_by_window": self.diagnostics_pruned_window,
                "truncated": self.diagnostics_dropped_capacity > 0 || self.diagnostics_pruned_window > 0,
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
            "setup_analysis": {
                "total": self.setup_analyses.len(),
                "tracking": self.virtual_outcomes.iter().filter(|item| matches!(item.outcome.as_str(), "WAITING_FOR_FILL" | "TRACKING")).count(),
                "counterfactual_summary": self.counterfactual_summary(),
                "target_first": self.virtual_outcomes.iter().filter(|item| item.outcome == "TARGET_FIRST").count(),
                "stop_first": self.virtual_outcomes.iter().filter(|item| item.outcome == "STOP_FIRST").count(),
                "latest": self.setup_analyses.iter().take(8).collect::<Vec<_>>(),
                "outcomes": self.virtual_outcomes.iter().take(8).collect::<Vec<_>>(),
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
            state.diagnose(
                "BTCUSDT",
                "candidate_check",
                None,
                Some(80),
                None,
                None,
                None,
            );
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
        assert_eq!(
            snapshot["signal_diagnostics_session"]["stages"]["rejected"],
            1
        );
    }

    #[test]
    fn session_aggregate_survives_recent_window_pruning() {
        let mut state = MarketState::new();
        state.diagnostic_session.record(&DiagnosticEvent {
            timestamp: now_ts() - DIAGNOSTICS_WINDOW_SECONDS - 1.0,
            symbol: "BTCUSDT".into(),
            stage: "qualified".into(),
            reason: None,
            score: Some(80),
            imbalance: Some(0.8),
            acceleration: Some(3.0),
            spread: Some(0.01),
        });
        state.diagnostics.push_back(DiagnosticEvent {
            timestamp: now_ts() - DIAGNOSTICS_WINDOW_SECONDS - 1.0,
            symbol: "BTCUSDT".into(),
            stage: "qualified".into(),
            reason: None,
            score: Some(80),
            imbalance: Some(0.8),
            acceleration: Some(3.0),
            spread: Some(0.01),
        });
        state.diagnose("ETHUSDT", "candidate_check", None, None, None, None, None);
        let export = state.diagnostics_export();
        assert_eq!(export["funnel_session"]["events"], 2);
        assert_eq!(export["funnel_session"]["stages"]["qualified"], 1);
        assert_eq!(export["funnel_recent"]["events"], 1);
        assert_eq!(export["metadata"]["diagnostics"]["pruned_by_window"], 1);
    }

    #[test]
    fn deduplicated_rejections_do_not_inflate_session_aggregate() {
        let mut state = MarketState::new();
        for _ in 0..10 {
            state.diagnose(
                "BTCUSDT",
                "rejected",
                Some("low imbalance"),
                None,
                Some(0.2),
                None,
                Some(0.01),
            );
        }
        assert_eq!(state.diagnostic_session.events, 1);
        assert_eq!(state.diagnostic_session.rejections["low imbalance"], 1);
    }

    #[test]
    fn export_reports_recent_buffer_truncation() {
        let mut state = MarketState::new();
        for _ in 0..=DIAGNOSTICS_MAXLEN {
            state.diagnose("BTCUSDT", "candidate_check", None, None, None, None, None);
        }
        let export = state.diagnostics_export();
        assert_eq!(export["funnel_session"]["events"], DIAGNOSTICS_MAXLEN + 1);
        assert_eq!(export["metadata"]["diagnostics"]["current_length"], DIAGNOSTICS_MAXLEN);
        assert_eq!(export["metadata"]["diagnostics"]["dropped_by_capacity"], 1);
        assert_eq!(export["metadata"]["diagnostics"]["truncated"], true);
    }
}
