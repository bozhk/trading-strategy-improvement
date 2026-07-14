use crate::config::SETTINGS;
use crate::execution::{entry_fill, estimated_round_trip_cost_pct, exit_fill, trade_pnl};
use crate::models::{
    now_ts, Absorption, ClosedTrade, OrderBook, PendingSignal, Position, SetupAnalysis, Side,
    TradeTick, WallTrack,
};
use crate::state::{MarketState, BTC_HISTORY_MAXLEN, STATE};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::time::Duration;

const WALL_CONFIRM_SECONDS: f64 = 3.0;
const WALL_DEPLETION_RATIO: f64 = 0.20;
const WALL_TAPE_MATCH_RATIO: f64 = 0.90;

const TAPE_WINDOW_SECONDS: f64 = 1.0;
const TAPE_ACCELERATION_MULTIPLIER: f64 = 3.0;
const TAPE_DOMINANCE_RATIO: f64 = 0.70;

const BTC_TREND_WINDOW_SECONDS: f64 = 60.0;
const BTC_MIN_TREND_COVERAGE_SECONDS: f64 = 45.0;
const BTC_MIN_TREND_PCT: f64 = 0.0002;

const BOOK_FRESHNESS_SECONDS: f64 = 2.0;

// Entry confirmation: the book imbalance must agree with the direction.
const ENTRY_IMBALANCE_LONG: f64 = 0.55;
const ENTRY_IMBALANCE_SHORT: f64 = 0.45;
const ENTRY_BREAKOUT_TOLERANCE_PCT: f64 = 0.0002;

// Exit management: opposing tape must persist for N consecutive ticks.
const REVERSAL_CONFIRM_TICKS: u32 = 3;
const REVERSAL_GRACE_SECONDS: f64 = 3.0;

const POSITION_STATUS_LOG_SECONDS: f64 = 5.0;
const ACCELERATION_CAP: f64 = 100.0;
const MIN_TAPE_BASELINE_NOTIONAL: f64 = 100.0;
const TAPE_BASELINE_CURRENT_SHARE: f64 = 0.05;

#[derive(Debug, Clone, Default)]
pub struct Tape {
    pub buy_dominance: f64,
    pub sell_dominance: f64,
    pub buy_acceleration: f64,
    pub sell_acceleration: f64,
    pub current_buy_notional: f64,
    pub current_sell_notional: f64,
    pub previous_buy_notional: f64,
    pub previous_sell_notional: f64,
    pub buy_baseline_valid: bool,
    pub sell_baseline_valid: bool,
    pub buy_accelerating: bool,
    pub sell_accelerating: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub imbalance: f64,
    pub strongest_bid_wall: Option<(f64, f64)>,
    pub strongest_ask_wall: Option<(f64, f64)>,
    pub wall_threshold: f64,
    pub flow: f64,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub spread: f64,
    pub freshness: f64,
    pub tape: Tape,
}

pub fn directional_tape(ticks: &VecDeque<TradeTick>, now: f64) -> Tape {
    let current_start = now - TAPE_WINDOW_SECONDS;
    let previous_start = now - TAPE_WINDOW_SECONDS * 2.0;
    let (mut current_buy, mut current_sell, mut previous_buy, mut previous_sell) =
        (0.0, 0.0, 0.0, 0.0);

    for tick in ticks {
        let notional = tick.price * tick.size;
        if tick.timestamp >= current_start {
            if tick.is_buy {
                current_buy += notional
            } else {
                current_sell += notional
            }
        } else if tick.timestamp >= previous_start {
            if tick.is_buy {
                previous_buy += notional
            } else {
                previous_sell += notional
            }
        }
    }

    let total = current_buy + current_sell;
    let buy_dominance = if total > 0.0 {
        current_buy / total
    } else {
        0.5
    };
    let sell_dominance = if total > 0.0 {
        current_sell / total
    } else {
        0.5
    };
    // A ratio is meaningful only when the previous window contains enough
    // absolute notional. This prevents an empty or microscopic baseline from
    // turning a single trade into an automatic acceleration confirmation.
    let adaptive_baseline = MIN_TAPE_BASELINE_NOTIONAL.max(total * TAPE_BASELINE_CURRENT_SHARE);
    let buy_baseline_valid = previous_buy >= adaptive_baseline;
    let sell_baseline_valid = previous_sell >= adaptive_baseline;
    let buy_acceleration = if buy_baseline_valid {
        (current_buy / previous_buy).min(ACCELERATION_CAP)
    } else {
        0.0
    };
    let sell_acceleration = if sell_baseline_valid {
        (current_sell / previous_sell).min(ACCELERATION_CAP)
    } else {
        0.0
    };

    Tape {
        buy_dominance,
        sell_dominance,
        buy_acceleration,
        sell_acceleration,
        current_buy_notional: current_buy,
        current_sell_notional: current_sell,
        previous_buy_notional: previous_buy,
        previous_sell_notional: previous_sell,
        buy_baseline_valid,
        sell_baseline_valid,
        buy_accelerating: buy_baseline_valid
            && current_buy > 0.0
            && buy_acceleration >= TAPE_ACCELERATION_MULTIPLIER
            && buy_dominance >= TAPE_DOMINANCE_RATIO,
        sell_accelerating: sell_baseline_valid
            && current_sell > 0.0
            && sell_acceleration >= TAPE_ACCELERATION_MULTIPLIER
            && sell_dominance >= TAPE_DOMINANCE_RATIO,
    }
}

pub fn book_metrics(book: &OrderBook, ticks: &VecDeque<TradeTick>, now: f64) -> Metrics {
    // Top 10 levels: bids descending, asks ascending.
    let bids: Vec<(f64, f64)> = book
        .bids
        .iter()
        .rev()
        .take(10)
        .map(|(p, q)| (p.0, *q))
        .collect();
    let asks: Vec<(f64, f64)> = book.asks.iter().take(10).map(|(p, q)| (p.0, *q)).collect();

    let bid_volume: f64 = bids.iter().map(|(_, q)| q).sum();
    let ask_volume: f64 = asks.iter().map(|(_, q)| q).sum();
    let total_volume = bid_volume + ask_volume;
    let imbalance = if total_volume > 0.0 {
        bid_volume / total_volume
    } else {
        0.5
    };

    let mut sizes: Vec<f64> = bids.iter().chain(asks.iter()).map(|(_, q)| *q).collect();
    sizes.sort_by(f64::total_cmp);
    let median_size = if sizes.is_empty() {
        0.0
    } else if sizes.len() % 2 == 1 {
        sizes[sizes.len() / 2]
    } else {
        (sizes[sizes.len() / 2 - 1] + sizes[sizes.len() / 2]) / 2.0
    };
    let wall_threshold = median_size * SETTINGS.wall_multiplier;

    let pick_wall = |levels: &[(f64, f64)]| -> Option<(f64, f64)> {
        levels
            .iter()
            .filter(|(_, q)| wall_threshold > 0.0 && *q >= wall_threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .copied()
    };

    let (mut buy_notional, mut sell_notional) = (0.0, 0.0);
    for tick in ticks.iter().filter(|t| now - t.timestamp <= 3.0) {
        let notional = tick.price * tick.size;
        if tick.is_buy {
            buy_notional += notional
        } else {
            sell_notional += notional
        }
    }
    let total_notional = buy_notional + sell_notional;
    let flow = if total_notional > 0.0 {
        buy_notional / total_notional
    } else {
        0.5
    };

    let (bid, ask) = book.quote();
    let spread = if bid > 0.0 && ask > 0.0 {
        (ask - bid) / bid * 100.0
    } else {
        999.0
    };

    Metrics {
        imbalance,
        strongest_bid_wall: pick_wall(&bids),
        strongest_ask_wall: pick_wall(&asks),
        wall_threshold,
        flow,
        bid,
        ask,
        mid: if bid > 0.0 && ask > 0.0 {
            (bid + ask) / 2.0
        } else {
            0.0
        },
        spread,
        freshness: now - book.updated_at,
        tape: directional_tape(ticks, now),
    }
}

fn wall_price_tolerance(book: &OrderBook, wall_price: f64) -> f64 {
    let (bid, ask) = book.quote();
    let live_spread = if bid > 0.0 && ask > 0.0 {
        ask - bid
    } else {
        0.0
    };
    live_spread.max(wall_price * 0.0001)
}

fn executed_quantity_at_wall(
    ticks: &VecDeque<TradeTick>,
    wall_side: &str,
    wall_price: f64,
    started_at: f64,
    now: f64,
    book: &OrderBook,
) -> f64 {
    let tolerance = wall_price_tolerance(book, wall_price);
    let mut executed = 0.0;
    for tick in ticks {
        if tick.timestamp < started_at || tick.timestamp > now {
            continue;
        }
        if wall_side == "ask" {
            // An ask wall can only be consumed by aggressive buyers.
            if tick.is_buy && tick.price >= wall_price - tolerance {
                executed += tick.size;
            }
        } else if !tick.is_buy && tick.price <= wall_price + tolerance {
            executed += tick.size;
        }
    }
    executed
}

fn new_wall_track(side: &'static str, price: f64, quantity: f64, now: f64) -> WallTrack {
    WallTrack {
        side,
        price,
        initial_size: quantity,
        peak_size: quantity,
        started_at: now,
        verified: false,
        triggered: false,
    }
}

/// Depletion alone is never absorption: the depleted base quantity must be
/// matched by aggressive executions at the wall AND confirmed by the tape.
#[allow(clippy::too_many_arguments)]
fn update_wall_track(
    wall_tracks: &mut std::collections::HashMap<(String, &'static str), WallTrack>,
    book: &OrderBook,
    ticks: &VecDeque<TradeTick>,
    symbol: &str,
    side: &'static str,
    candidate: Option<(f64, f64)>,
    now: f64,
) -> Option<Absorption> {
    let key = (symbol.to_string(), side);

    let Some(track) = wall_tracks.get_mut(&key) else {
        if let Some((price, quantity)) = candidate {
            wall_tracks.insert(key, new_wall_track(side, price, quantity, now));
        }
        return None;
    };

    let levels = if side == "ask" {
        &book.asks
    } else {
        &book.bids
    };
    let current_size = levels
        .get(&ordered_float::OrderedFloat(track.price))
        .copied()
        .unwrap_or(0.0);

    if !track.verified {
        let matches = candidate
            .map(|(price, _)| {
                (price - track.price).abs() <= wall_price_tolerance(book, track.price)
            })
            .unwrap_or(false);
        if !matches {
            if let Some((price, quantity)) = candidate {
                wall_tracks.insert(key, new_wall_track(side, price, quantity, now));
            } else {
                wall_tracks.remove(&key);
            }
            return None;
        }
        let (_, candidate_size) = candidate.unwrap();

        // A meaningful refill restarts continuous verification.
        if candidate_size > track.peak_size * 1.05 {
            track.initial_size = candidate_size;
            track.peak_size = candidate_size;
            track.started_at = now;
            return None;
        }
        track.peak_size = track.peak_size.max(candidate_size);
        let depletion = track.initial_size - current_size;
        if depletion >= track.initial_size * WALL_DEPLETION_RATIO {
            wall_tracks.remove(&key);
            return None;
        }
        if now - track.started_at >= WALL_CONFIRM_SECONDS {
            track.verified = true;
        } else {
            return None;
        }
    }

    if track.triggered {
        return None;
    }

    let initial_size = track.initial_size;
    let depleted_quantity = (initial_size - current_size).max(0.0);
    let depletion_ratio = if initial_size > 0.0 {
        depleted_quantity / initial_size
    } else {
        0.0
    };
    let executed_quantity =
        executed_quantity_at_wall(ticks, side, track.price, track.started_at, now, book);
    let matched_ratio = if depleted_quantity > 0.0 {
        executed_quantity / depleted_quantity
    } else {
        0.0
    };

    let tape = directional_tape(ticks, now);
    let tape_confirmed = if side == "ask" {
        tape.buy_accelerating
    } else {
        tape.sell_accelerating
    };

    let absorbed = depletion_ratio >= WALL_DEPLETION_RATIO
        && executed_quantity >= depleted_quantity * WALL_TAPE_MATCH_RATIO
        && tape_confirmed;

    let result = Absorption {
        side,
        price: track.price,
        initial_size,
        current_size,
        depleted_quantity,
        depletion_ratio,
        executed_quantity,
        matched_ratio,
        verified_for: now - track.started_at,
        absorbed,
    };

    if absorbed {
        track.triggered = true;
    }

    // Remove old observations that are no longer actionable.
    if !absorbed
        && current_size <= 0.0
        && (depletion_ratio < WALL_DEPLETION_RATIO
            || executed_quantity < depleted_quantity * WALL_TAPE_MATCH_RATIO)
    {
        wall_tracks.remove(&key);
    }

    Some(result)
}

fn record_btc_mid(state: &mut MarketState, now: f64) {
    let Some(book) = state.books.get("BTCUSDT") else {
        return;
    };
    let (bid, ask) = book.quote();
    if bid <= 0.0 || ask <= 0.0 {
        return;
    }
    let mid = (bid + ask) / 2.0;
    let history = &mut state.btc_mid_history;
    if history
        .back()
        .map(|(ts, _)| now - ts >= 0.25)
        .unwrap_or(true)
    {
        if history.len() >= BTC_HISTORY_MAXLEN {
            history.pop_front();
        }
        history.push_back((now, mid));
    }
    let cutoff = now - (BTC_TREND_WINDOW_SECONDS + 5.0);
    while history.front().map(|(ts, _)| *ts < cutoff).unwrap_or(false) {
        history.pop_front();
    }
}

#[derive(Debug, Clone)]
pub struct BtcTrend {
    pub ready: bool,
    pub direction: &'static str,
    pub change_pct: f64,
    pub coverage: f64,
}

fn btc_trend(state: &MarketState, now: f64) -> BtcTrend {
    let cutoff = now - BTC_TREND_WINDOW_SECONDS;
    let samples: Vec<(f64, f64)> = state
        .btc_mid_history
        .iter()
        .copied()
        .filter(|(ts, _)| *ts >= cutoff)
        .collect();

    if samples.len() < 2 {
        return BtcTrend {
            ready: false,
            direction: "UNKNOWN",
            change_pct: 0.0,
            coverage: 0.0,
        };
    }
    let coverage = samples[samples.len() - 1].0 - samples[0].0;
    let start_price = samples[0].1;
    let end_price = samples[samples.len() - 1].1;
    let change_pct = if start_price > 0.0 {
        (end_price - start_price) / start_price
    } else {
        0.0
    };

    let (direction, ready) = if coverage < BTC_MIN_TREND_COVERAGE_SECONDS {
        ("UNKNOWN", false)
    } else if change_pct >= BTC_MIN_TREND_PCT {
        ("UP", true)
    } else if change_pct <= -BTC_MIN_TREND_PCT {
        ("DOWN", true)
    } else {
        ("FLAT", true)
    };
    BtcTrend {
        ready,
        direction,
        change_pct,
        coverage,
    }
}

fn btc_allows(state: &MarketState, symbol: &str, side: Side, now: f64) -> (bool, BtcTrend) {
    let trend = btc_trend(state, now);
    if symbol == "BTCUSDT" {
        return (true, trend);
    }
    // Fail closed until a genuine 45-60 second BTC trend sample exists.
    if !trend.ready {
        return (false, trend);
    }
    let allowed = match side {
        Side::LONG => trend.direction == "UP",
        Side::SHORT => trend.direction == "DOWN",
    };
    (allowed, trend)
}

#[allow(clippy::too_many_arguments)]
fn open_position(
    state: &mut MarketState,
    symbol: &str,
    side: Side,
    bid: f64,
    ask: f64,
    now: f64,
    stop_price: f64,
    target_price: f64,
    snapshot: Value,
    context: &str,
) -> bool {
    let touch = if side == Side::LONG { ask } else { bid };
    let stop_distance = (touch - stop_price).abs();
    if stop_distance <= 0.0 {
        return false;
    }
    let risk_budget = SETTINGS.account_equity * SETTINGS.risk_per_trade_pct;
    let quantity = (risk_budget / stop_distance).min(SETTINGS.max_position_notional / touch);
    let fill = entry_fill(side, bid, ask, quantity);
    let actual_risk = (fill.price - stop_price).abs();

    state.positions.insert(
        symbol.to_string(),
        Position {
            symbol: symbol.to_string(),
            side,
            entry: fill.price,
            quantity,
            opened_at: now,
            mark: (bid + ask) / 2.0,
            stop_price,
            target_price,
            risk_per_unit: actual_risk,
            entry_fee: fill.fee,
            entry_slippage_cost: fill.slippage_cost,
            signal_snapshot: snapshot,
            unrealized: 0.0,
            breakeven_armed: false,
            peak_return: 0.0,
            trough_return: 0.0,
            mfe: 0.0,
            mae: 0.0,
            reversal_streak: 0,
            last_status_log: now,
        },
    );
    let detail = if context.is_empty() {
        String::new()
    } else {
        format!(" · {context}")
    };
    let message = format!(
        "Paper {} @ {:.6} · stop {:.6} · target {:.6} · risk {:.2} USDT{detail}",
        side.as_str(),
        fill.price,
        stop_price,
        target_price,
        risk_budget,
    );
    state.log("ENTRY", &message, symbol, 0.0);
    true
}

fn close_position(
    state: &mut MarketState,
    mut pos: Position,
    metrics: &Metrics,
    reason: &str,
    now: f64,
    detail: &str,
) {
    let fill = exit_fill(pos.side, metrics.bid, metrics.ask, pos.quantity);
    let (gross, pnl) = trade_pnl(
        pos.side,
        pos.entry,
        fill.price,
        pos.quantity,
        pos.entry_fee,
        fill.fee,
    );
    let total_fees = pos.entry_fee + fill.fee;
    let symbol = std::mem::take(&mut pos.symbol);

    state.push_closed(ClosedTrade {
        symbol: symbol.clone(),
        side: pos.side.as_str(),
        entry: pos.entry,
        exit: fill.price,
        pnl,
        gross_pnl: gross,
        fees: total_fees,
        slippage_cost: pos.entry_slippage_cost + fill.slippage_cost,
        reason: reason.to_string(),
        opened_at: pos.opened_at,
        closed_at: now,
        mfe: pos.mfe,
        mae: pos.mae,
        signal_snapshot: pos.signal_snapshot.clone(),
        exit_snapshot: json!({
            "bid": metrics.bid, "ask": metrics.ask,
            "imbalance": metrics.imbalance, "flow": metrics.flow,
        }),
    });
    let cooldown = if pnl < 0.0 {
        SETTINGS.loss_cooldown_seconds
    } else {
        SETTINGS.cooldown_seconds
    };
    state.cooldowns.insert(symbol.clone(), now + cooldown);
    let held = now - pos.opened_at;
    let outcome = if pnl > 0.0 { "WIN" } else { "LOSS" };
    let extra = if detail.is_empty() {
        String::new()
    } else {
        format!(" · {detail}")
    };
    let message = format!(
        "{reason} [{outcome}] {} · net {pnl:+.2} USDT · gross {gross:+.2} · fees {total_fees:.2} · held {held:.1}s · MFE {:+.2} / MAE {:+.2}{extra}",
        pos.side.as_str(), pos.mfe, pos.mae,
    );
    state.log("EXIT", &message, &symbol, 0.0);
}

fn position_return(pos: &Position, mark: f64) -> f64 {
    if pos.entry == 0.0 {
        return 0.0;
    }
    ((mark - pos.entry) / pos.entry) * pos.side.direction()
}

/// Returns the position back unless it was closed.
fn manage_position(
    state: &mut MarketState,
    mut pos: Position,
    metrics: &Metrics,
    now: f64,
) -> Option<Position> {
    let executable = if pos.side == Side::LONG {
        metrics.bid
    } else {
        metrics.ask
    };
    pos.mark = executable;
    let estimated_exit_fee = executable * pos.quantity * SETTINGS.taker_fee_pct;
    let (_, unrealized) = trade_pnl(
        pos.side,
        pos.entry,
        executable,
        pos.quantity,
        pos.entry_fee,
        estimated_exit_fee,
    );
    pos.unrealized = unrealized;
    let gross_return = position_return(&pos, executable);
    pos.peak_return = pos.peak_return.max(gross_return);
    pos.trough_return = pos.trough_return.min(gross_return);
    pos.mfe = pos.mfe.max(unrealized);
    pos.mae = pos.mae.min(unrealized);
    let r_now = if pos.risk_per_unit > 0.0 {
        (executable - pos.entry) * pos.side.direction() / pos.risk_per_unit
    } else {
        0.0
    };

    let stop_hit = if pos.side == Side::LONG {
        executable <= pos.stop_price
    } else {
        executable >= pos.stop_price
    };
    let target_hit = if pos.side == Side::LONG {
        executable >= pos.target_price
    } else {
        executable <= pos.target_price
    };
    if stop_hit {
        close_position(state, pos, metrics, "STRUCTURAL STOP", now, "");
        return None;
    }
    if target_hit {
        let detail = format!("target {:.1}R reached", SETTINGS.target_r_multiple);
        close_position(state, pos, metrics, "R-MULTIPLE TARGET", now, &detail);
        return None;
    }

    if r_now >= SETTINGS.breakeven_arm_r {
        pos.breakeven_armed = true;
    }
    if pos.breakeven_armed && r_now <= 0.12 {
        close_position(state, pos, metrics, "COST-PROTECTED BREAKEVEN", now, "");
        return None;
    }

    let tape_reversed = (pos.side == Side::LONG && metrics.tape.sell_accelerating)
        || (pos.side == Side::SHORT && metrics.tape.buy_accelerating);
    pos.reversal_streak = if tape_reversed {
        pos.reversal_streak + 1
    } else {
        0
    };
    if pos.reversal_streak >= REVERSAL_CONFIRM_TICKS
        && now - pos.opened_at >= REVERSAL_GRACE_SECONDS
    {
        let detail = format!("opposing tape persisted {} ticks", pos.reversal_streak);
        close_position(
            state,
            pos,
            metrics,
            "CONFIRMED FLOW INVALIDATION",
            now,
            &detail,
        );
        return None;
    }

    if pos.risk_per_unit > 0.0 {
        let peak_r = pos.peak_return * pos.entry / pos.risk_per_unit;
        if peak_r >= SETTINGS.trail_arm_r && r_now <= peak_r - SETTINGS.trail_giveback_r {
            let detail = format!("peak {peak_r:.2}R → {r_now:.2}R");
            close_position(state, pos, metrics, "R-BASED TRAIL", now, &detail);
            return None;
        }
    }

    if now - pos.last_status_log >= POSITION_STATUS_LOG_SECONDS {
        pos.last_status_log = now;
        let held = now - pos.opened_at;
        let flow = if pos.side == Side::LONG {
            metrics.tape.buy_dominance
        } else {
            metrics.tape.sell_dominance
        };
        let be = if pos.breakeven_armed {
            " · BE-armed"
        } else {
            ""
        };
        let message = format!(
            "{} {held:.0}s · mark {:.6} ({:+.3}%) · uPnL {:+.2} USDT · peak {:+.3}% · flow-with-us {:.0}% · imb {:.2} · spread {:.3}%{be} · {r_now:+.2}R · stop {:.6}",
            pos.side.as_str(), metrics.mid, gross_return * 100.0, pos.unrealized,
            pos.peak_return * 100.0, flow * 100.0, metrics.imbalance, metrics.spread, pos.stop_price,
        );
        let symbol = pos.symbol.clone();
        state.log("POSITION", &message, &symbol, 0.0);
    }
    Some(pos)
}

fn attempt_entry(
    state: &mut MarketState,
    symbol: &str,
    side: Side,
    now: f64,
    metrics: &Metrics,
    absorption: &Absorption,
) -> (bool, BtcTrend) {
    let trend = btc_trend(state, now);
    let (
        acceleration,
        acceleration_current_notional,
        acceleration_baseline_notional,
        acceleration_baseline_valid,
    ) = if side == Side::LONG {
            (
                metrics.tape.buy_acceleration,
                metrics.tape.current_buy_notional,
                metrics.tape.previous_buy_notional,
                metrics.tape.buy_baseline_valid,
            )
        } else {
            (
                metrics.tape.sell_acceleration,
                metrics.tape.current_sell_notional,
                metrics.tape.previous_sell_notional,
                metrics.tape.sell_baseline_valid,
            )
        };
    state.diagnose(
        symbol,
        "candidate_check",
        None,
        None,
        Some(metrics.imbalance),
        Some(acceleration),
        Some(metrics.spread),
    );
    if state.positions.len() >= SETTINGS.max_open_positions {
        state.reject(symbol, "portfolio position limit");
        return (false, trend);
    }
    let daily_pnl: f64 = state
        .closed
        .iter()
        .filter(|t| now - t.closed_at <= 86_400.0)
        .map(|t| t.pnl)
        .sum();
    if daily_pnl <= -SETTINGS.account_equity * SETTINGS.max_daily_loss_pct {
        state.reject(symbol, "daily loss circuit breaker");
        return (false, trend);
    }
    let recent: Vec<f64> = state
        .closed
        .iter()
        .take(SETTINGS.max_consecutive_losses)
        .map(|t| t.pnl)
        .collect();
    if recent.len() == SETTINGS.max_consecutive_losses && recent.iter().all(|p| *p < 0.0) {
        state.reject(symbol, "loss-streak circuit breaker");
        return (false, trend);
    }

    let (btc_allowed, trend) = btc_allows(state, symbol, side, now);
    let directional_imbalance = if side == Side::LONG {
        metrics.imbalance >= ENTRY_IMBALANCE_LONG
    } else {
        metrics.imbalance <= ENTRY_IMBALANCE_SHORT
    };
    let tape_ok = if side == Side::LONG {
        metrics.tape.buy_accelerating
    } else {
        metrics.tape.sell_accelerating
    };
    let score: i64 = 25 * i64::from(directional_imbalance)
        + 20 * i64::from(btc_allowed)
        + 20 * i64::from(tape_ok)
        + 15 * i64::from(absorption.matched_ratio >= 0.95)
        + 10 * i64::from(metrics.spread <= SETTINGS.max_spread_pct * 0.65)
        + 10 * i64::from(metrics.freshness <= 1.0);
    if score < SETTINGS.min_confluence_score {
        let reason = format!(
            "confluence {score}/100 below {}",
            SETTINGS.min_confluence_score
        );
        state.reject(symbol, &reason);
        state.diagnose(
            symbol,
            "rejected",
            Some(&reason),
            Some(score),
            Some(metrics.imbalance),
            Some(acceleration),
            Some(metrics.spread),
        );
        return (false, trend);
    }

    let (bid, ask) = (metrics.bid, metrics.ask);
    let wall = absorption.price;

    // State machine: qualify -> wait for breakout -> hold retest -> enter.
    let needs_new = state
        .pending_signals
        .get(symbol)
        .map(|p| p.side != side)
        .unwrap_or(true);
    if needs_new {
        state.pending_signals.insert(
            symbol.to_string(),
            PendingSignal {
                side,
                wall,
                created: now,
                broken: false,
                hold_ticks: 0,
                score,
                absorption: absorption.clone(),
            },
        );
        let message = format!(
            "{} absorption qualified {score}/100; waiting breakout + retest",
            side.as_str()
        );
        state.log("SETUP", &message, symbol, 0.0);
        state.diagnose(
            symbol,
            "qualified",
            None,
            Some(score),
            Some(metrics.imbalance),
            Some(acceleration),
            Some(metrics.spread),
        );
        return (false, trend);
    }

    if state
        .pending_signals
        .get(symbol)
        .map(|pending| now - pending.created > SETTINGS.signal_expiry_seconds)
        .unwrap_or(false)
    {
        state.pending_signals.remove(symbol);
        state.reject(symbol, "setup expired before retest");
        state.diagnose(
            symbol,
            "rejected",
            Some("setup expired before retest"),
            Some(score),
            Some(metrics.imbalance),
            Some(acceleration),
            Some(metrics.spread),
        );
        return (false, trend);
    }

    let broke = if side == Side::LONG {
        bid > wall * (1.0 + ENTRY_BREAKOUT_TOLERANCE_PCT)
    } else {
        ask < wall * (1.0 - ENTRY_BREAKOUT_TOLERANCE_PCT)
    };
    let (newly_broken, broken, hold_ticks) = {
        let pending = state.pending_signals.get_mut(symbol).unwrap();
        let newly_broken = broke && !pending.broken;
        pending.broken = pending.broken || broke;
        if !pending.broken {
            (newly_broken, false, pending.hold_ticks)
        } else {
            let tolerance = wall * SETTINGS.retest_tolerance_pct;
            let retest = if side == Side::LONG {
                bid >= wall - tolerance && bid <= wall + tolerance * 2.0
            } else {
                ask <= wall + tolerance && ask >= wall - tolerance * 2.0
            };
            pending.hold_ticks = if retest { pending.hold_ticks + 1 } else { 0 };
            (newly_broken, true, pending.hold_ticks)
        }
    };
    if newly_broken {
        state.diagnose(
            symbol,
            "breakout",
            None,
            Some(score),
            Some(metrics.imbalance),
            Some(acceleration),
            Some(metrics.spread),
        );
    }
    if !broken || hold_ticks < SETTINGS.retest_hold_ticks {
        return (false, trend);
    }
    state.diagnose(
        symbol,
        "retest",
        None,
        Some(score),
        Some(metrics.imbalance),
        Some(acceleration),
        Some(metrics.spread),
    );
    // This is the single actionable attempt in the pending setup lifecycle.
    // The pending signal is removed after the economics decision below, so the
    // stage cannot be emitted repeatedly on subsequent market updates.
    state.diagnose(
        symbol,
        "entry_attempt",
        None,
        Some(score),
        Some(metrics.imbalance),
        Some(acceleration),
        Some(metrics.spread),
    );

    let cost_pct = estimated_round_trip_cost_pct(bid, ask);
    let raw_stop_pct = SETTINGS.min_stop_pct.max(
        SETTINGS
            .max_stop_pct
            .min(SETTINGS.structure_buffer_pct + metrics.spread / 100.0 * 2.0),
    );
    let touch = if side == Side::LONG { ask } else { bid };
    let stop = if side == Side::LONG {
        wall * (1.0 - raw_stop_pct)
    } else {
        wall * (1.0 + raw_stop_pct)
    };
    let risk = (touch - stop).abs();
    let target = touch + risk * SETTINGS.target_r_multiple * side.direction();
    let net_reward = (target - touch).abs() / touch - cost_pct;
    let net_risk = risk / touch + cost_pct;
    let net_rr = if net_risk > 0.0 {
        net_reward / net_risk
    } else {
        0.0
    };
    let target_pct = (target - touch).abs() / touch * 100.0;
    let stop_pct = risk / touch * 100.0;
    let cost_pct_display = cost_pct * 100.0;
    let cost_coverage = if cost_pct_display > 0.0 {
        target_pct / cost_pct_display
    } else {
        0.0
    };
    let stop_cost_ratio = if cost_pct_display > 0.0 {
        stop_pct / cost_pct_display
    } else {
        0.0
    };
    let rejected_by_rr = net_reward <= 0.0 || net_rr < SETTINGS.min_net_reward_risk;
    let reason = if rejected_by_rr {
        format!("after-cost R/R {net_rr:.2}")
    } else {
        "entry accepted".to_string()
    };
    let setup_analysis = SetupAnalysis {
        id: 0,
        timestamp: now,
        symbol: symbol.to_string(),
        side: side.as_str().to_string(),
        decision: if rejected_by_rr { "REJECT" } else { "ENTRY" }.into(),
        reason: reason.clone(),
        wall,
        breakout_price: wall * (1.0 + ENTRY_BREAKOUT_TOLERANCE_PCT * side.direction()),
        retest_price: touch,
        entry: touch,
        stop,
        target,
        stop_pct,
        target_pct,
        gross_rr: if risk > 0.0 {
            (target - touch).abs() / risk
        } else {
            0.0
        },
        cost_pct: cost_pct_display,
        cost_coverage,
        stop_cost_ratio,
        net_rr,
        required_net_rr: SETTINGS.min_net_reward_risk,
        score,
        score_required: SETTINGS.min_confluence_score,
        score_breakdown: json!({
            "directional_imbalance": 25 * i64::from(directional_imbalance),
            "btc_alignment": 20 * i64::from(btc_allowed),
            "tape_acceleration": 20 * i64::from(tape_ok),
            "absorption_match": 15 * i64::from(absorption.matched_ratio >= 0.95),
            "tight_spread": 10 * i64::from(metrics.spread <= SETTINGS.max_spread_pct * 0.65),
            "fresh_book": 10 * i64::from(metrics.freshness <= 1.0),
        }),
        filter_actual: net_rr,
        filter_required: SETTINGS.min_net_reward_risk,
        filter_gap: net_rr - SETTINGS.min_net_reward_risk,
        imbalance: metrics.imbalance,
        acceleration,
        acceleration_current_notional,
        acceleration_baseline_notional,
        acceleration_baseline_valid,
        spread: metrics.spread,
        freshness: metrics.freshness,
        absorption_initial_size: absorption.initial_size,
        absorption_current_size: absorption.current_size,
        absorption_depleted_quantity: absorption.depleted_quantity,
        absorption_executed_quantity: absorption.executed_quantity,
        absorption_matched: absorption.matched_ratio,
        btc_trend: trend.direction.to_string(),
    };
    state.record_setup(setup_analysis, rejected_by_rr);
    if rejected_by_rr {
        state.pending_signals.remove(symbol);
        state.reject(symbol, &reason);
        state.diagnose(
            symbol,
            "rejected",
            Some(&reason),
            Some(score),
            Some(metrics.imbalance),
            Some(acceleration),
            Some(metrics.spread),
        );
        return (false, trend);
    }

    let sequence = state.books.get(symbol).map(|b| b.sequence).unwrap_or(0);
    let snapshot = json!({
        "wall": wall, "score": score, "imbalance": metrics.imbalance,
        "flow": metrics.flow, "btc_trend": trend.direction,
        "spread_pct": metrics.spread, "cost_pct": cost_pct,
        "net_rr": net_rr, "sequence": sequence,
    });
    let context = format!("breakout-retest · score {score}/100 · net R/R {net_rr:.2}");
    let opened = open_position(
        state, symbol, side, bid, ask, now, stop, target, snapshot, &context,
    );
    state.pending_signals.remove(symbol);
    if opened {
        state.diagnose(
            symbol,
            "entered",
            None,
            Some(score),
            Some(metrics.imbalance),
            Some(acceleration),
            Some(metrics.spread),
        );
    }
    (opened, trend)
}

fn metrics_to_radar(
    symbol: &str,
    metrics: &Metrics,
    status: &str,
    trend: &BtcTrend,
    bid_absorption: &Option<Absorption>,
    ask_absorption: &Option<Absorption>,
) -> Value {
    let absorption_json = |a: &Option<Absorption>| -> Value {
        match a {
            None => Value::Null,
            Some(a) => json!({
                "side": a.side, "price": a.price, "initial_size": a.initial_size,
                "current_size": a.current_size, "depleted_quantity": a.depleted_quantity,
                "depletion_ratio": a.depletion_ratio, "executed_quantity": a.executed_quantity,
                "matched_ratio": a.matched_ratio, "verified_for": a.verified_for,
                "absorbed": a.absorbed,
            }),
        }
    };
    json!({
        "symbol": symbol,
        "status": status,
        "price": metrics.mid,
        "imbalance": metrics.imbalance,
        "bid_wall": metrics.strongest_bid_wall.is_some(),
        "ask_wall": metrics.strongest_ask_wall.is_some(),
        "wall_threshold": metrics.wall_threshold,
        "flow": metrics.flow,
        "bid": metrics.bid,
        "ask": metrics.ask,
        "mid": metrics.mid,
        "spread": metrics.spread,
        "freshness": metrics.freshness,
        "buy_dominance": metrics.tape.buy_dominance,
        "sell_dominance": metrics.tape.sell_dominance,
        "buy_acceleration": metrics.tape.buy_acceleration,
        "sell_acceleration": metrics.tape.sell_acceleration,
        "current_buy_notional": metrics.tape.current_buy_notional,
        "current_sell_notional": metrics.tape.current_sell_notional,
        "previous_buy_notional": metrics.tape.previous_buy_notional,
        "previous_sell_notional": metrics.tape.previous_sell_notional,
        "buy_baseline_valid": metrics.tape.buy_baseline_valid,
        "sell_baseline_valid": metrics.tape.sell_baseline_valid,
        "buy_accelerating": metrics.tape.buy_accelerating,
        "sell_accelerating": metrics.tape.sell_accelerating,
        "bid_absorption": absorption_json(bid_absorption),
        "ask_absorption": absorption_json(ask_absorption),
        "btc_trend": trend.direction,
        "btc_trend_pct": trend.change_pct * 100.0,
        "btc_trend_coverage": trend.coverage,
    })
}

pub fn evaluate_symbol(state: &mut MarketState, symbol: &str, now: f64) {
    let Some(book) = state.books.get(symbol) else {
        return;
    };
    let empty = VecDeque::new();
    let ticks = state.trades.get(symbol).unwrap_or(&empty);
    let metrics = book_metrics(book, ticks, now);
    state.update_virtual_outcomes(symbol, metrics.bid, metrics.ask, now);

    // Wall tracking needs split borrows: books/trades read-only, tracks mutable.
    let (bid_absorption, ask_absorption) = {
        let MarketState {
            books,
            trades,
            wall_tracks,
            ..
        } = state;
        let book = books.get(symbol).unwrap();
        let empty = VecDeque::new();
        let ticks = trades.get(symbol).unwrap_or(&empty);
        (
            update_wall_track(
                wall_tracks,
                book,
                ticks,
                symbol,
                "bid",
                metrics.strongest_bid_wall,
                now,
            ),
            update_wall_track(
                wall_tracks,
                book,
                ticks,
                symbol,
                "ask",
                metrics.strongest_ask_wall,
                now,
            ),
        )
    };

    let mut trend = btc_trend(state, now);

    if let Some(pos) = state.positions.remove(symbol) {
        let status = pos.side.as_str();
        if let Some(pos) = manage_position(state, pos, &metrics, now) {
            state.positions.insert(symbol.to_string(), pos);
        }
        let radar = metrics_to_radar(
            symbol,
            &metrics,
            status,
            &trend,
            &bid_absorption,
            &ask_absorption,
        );
        state.metrics.insert(symbol.to_string(), radar);
        return;
    }

    if now < state.cooldowns.get(symbol).copied().unwrap_or(0.0) {
        let radar = metrics_to_radar(
            symbol,
            &metrics,
            "COOLDOWN",
            &trend,
            &bid_absorption,
            &ask_absorption,
        );
        state.metrics.insert(symbol.to_string(), radar);
        return;
    }

    if metrics.freshness > BOOK_FRESHNESS_SECONDS {
        state.diagnose(
            symbol,
            "rejected",
            Some("stale book"),
            None,
            Some(metrics.imbalance),
            None,
            Some(metrics.spread),
        );
        state.log("SKIP", "TIMEOUT / SKIPPED · stale book", symbol, 8.0);
        let radar = metrics_to_radar(
            symbol,
            &metrics,
            "WATCH",
            &trend,
            &bid_absorption,
            &ask_absorption,
        );
        state.metrics.insert(symbol.to_string(), radar);
        return;
    }
    if metrics.spread > SETTINGS.max_spread_pct {
        state.diagnose(
            symbol,
            "rejected",
            Some("spread expanded"),
            None,
            Some(metrics.imbalance),
            None,
            Some(metrics.spread),
        );
        state.log("SKIP", "TIMEOUT / SKIPPED · spread expanded", symbol, 8.0);
        let radar = metrics_to_radar(
            symbol,
            &metrics,
            "WATCH",
            &trend,
            &bid_absorption,
            &ask_absorption,
        );
        state.metrics.insert(symbol.to_string(), radar);
        return;
    }

    // A qualifying absorption starts a state machine. Subsequent ticks advance
    // the same candidate through breakout and held retest; no instant entries.
    let mut entered = false;
    if let Some(a) = ask_absorption.as_ref().filter(|a| a.absorbed) {
        let absorption = a.clone();
        (entered, trend) = attempt_entry(state, symbol, Side::LONG, now, &metrics, &absorption);
    } else if let Some(a) = bid_absorption.as_ref().filter(|a| a.absorbed) {
        let absorption = a.clone();
        (entered, trend) = attempt_entry(state, symbol, Side::SHORT, now, &metrics, &absorption);
    } else if let Some(pending) = state.pending_signals.get(symbol) {
        let (side, absorption) = (pending.side, pending.absorption.clone());
        (entered, trend) = attempt_entry(state, symbol, side, now, &metrics, &absorption);
    }

    let status = if entered {
        state
            .positions
            .get(symbol)
            .map(|p| p.side.as_str())
            .unwrap_or("WATCH")
    } else {
        "WATCH"
    };
    if !entered {
        if (metrics.imbalance - 0.5).abs() < 0.1 {
            state.diagnose(
                symbol,
                "rejected",
                Some("low imbalance"),
                None,
                Some(metrics.imbalance),
                None,
                Some(metrics.spread),
            );
            state.log("SKIP", "TIMEOUT / SKIPPED · low imbalance", symbol, 8.0);
        } else if !metrics.tape.buy_accelerating && !metrics.tape.sell_accelerating {
            let acceleration = metrics
                .tape
                .buy_acceleration
                .max(metrics.tape.sell_acceleration);
            state.diagnose(
                symbol,
                "rejected",
                Some("no 3x tape acceleration"),
                None,
                Some(metrics.imbalance),
                Some(acceleration),
                Some(metrics.spread),
            );
            state.log(
                "SKIP",
                "TIMEOUT / SKIPPED · no 3x tape acceleration",
                symbol,
                8.0,
            );
        }
    }
    let radar = metrics_to_radar(
        symbol,
        &metrics,
        status,
        &trend,
        &bid_absorption,
        &ask_absorption,
    );
    state.metrics.insert(symbol.to_string(), radar);
}

pub async fn brain_loop() {
    let mut session: u64 = 0;
    loop {
        let now = now_ts();
        {
            let mut state = STATE.lock();
            if session != state.session {
                state.wall_tracks.clear();
                state.pending_signals.clear();
                state.btc_mid_history.clear();
                session = state.session;
            }
            record_btc_mid(&mut state, now);

            // BTC is evaluated first so its latest market state feeds the
            // global trend filter for every other symbol.
            let mut symbols: Vec<String> = state.books.keys().cloned().collect();
            symbols.sort();
            if symbols.iter().any(|s| s == "BTCUSDT") {
                evaluate_symbol(&mut state, "BTCUSDT", now);
            }
            for symbol in symbols.iter().filter(|s| s.as_str() != "BTCUSDT") {
                evaluate_symbol(&mut state, symbol, now);
            }
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::directional_tape;
    use crate::models::TradeTick;
    use std::collections::VecDeque;

    fn tick(timestamp: f64, is_buy: bool, notional: f64) -> TradeTick {
        TradeTick {
            timestamp,
            is_buy,
            price: 1.0,
            size: notional,
        }
    }

    #[test]
    fn empty_baseline_never_confirms_acceleration() {
        let ticks = VecDeque::from([tick(9.5, true, 10_000.0)]);
        let tape = directional_tape(&ticks, 10.0);
        assert!(!tape.buy_baseline_valid);
        assert_eq!(tape.buy_acceleration, 0.0);
        assert!(!tape.buy_accelerating);
    }

    #[test]
    fn valid_three_x_buy_acceleration_is_confirmed_with_dominance() {
        let ticks = VecDeque::from([
            tick(8.5, true, 1_000.0),
            tick(9.5, true, 3_200.0),
            tick(9.5, false, 300.0),
        ]);
        let tape = directional_tape(&ticks, 10.0);
        assert!(tape.buy_baseline_valid);
        assert!(tape.buy_acceleration >= 3.0);
        assert!(tape.buy_dominance >= 0.70);
        assert!(tape.buy_accelerating);
    }

    #[test]
    fn microscopic_baseline_is_rejected() {
        let ticks = VecDeque::from([tick(8.5, true, 1.0), tick(9.5, true, 5_000.0)]);
        let tape = directional_tape(&ticks, 10.0);
        assert!(!tape.buy_baseline_valid);
        assert!(!tape.buy_accelerating);
    }

    #[test]
    fn acceleration_without_directional_dominance_is_rejected() {
        let ticks = VecDeque::from([
            tick(8.5, true, 1_000.0),
            tick(9.5, true, 3_100.0),
            tick(9.5, false, 2_000.0),
        ]);
        let tape = directional_tape(&ticks, 10.0);
        assert!(tape.buy_baseline_valid);
        assert!(tape.buy_acceleration >= 3.0);
        assert!(!tape.buy_accelerating);
    }
}
