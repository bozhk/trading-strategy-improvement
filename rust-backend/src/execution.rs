use crate::config::SETTINGS;
use crate::models::Side;

#[derive(Debug, Clone, Copy)]
pub struct Fill {
    pub price: f64,
    pub fee: f64,
    pub slippage_cost: f64,
}

/// Marketable entry: LONG pays the ask; SHORT sells the bid.
pub fn entry_fill(side: Side, bid: f64, ask: f64, quantity: f64) -> Fill {
    let touch = if side == Side::Long { ask } else { bid };
    let price = touch * (1.0 + side.direction() * SETTINGS.slippage_pct);
    Fill {
        price,
        fee: price * quantity * SETTINGS.taker_fee_pct,
        slippage_cost: (price - touch).abs() * quantity,
    }
}

/// Liquidating exit crosses to the opposite side of the spread.
pub fn exit_fill(side: Side, bid: f64, ask: f64, quantity: f64) -> Fill {
    let touch = if side == Side::Long { bid } else { ask };
    let price = touch * (1.0 - side.direction() * SETTINGS.slippage_pct);
    Fill {
        price,
        fee: price * quantity * SETTINGS.taker_fee_pct,
        slippage_cost: (price - touch).abs() * quantity,
    }
}

/// Returns (gross, net) PnL with explicit round-trip fees.
pub fn trade_pnl(
    side: Side,
    entry: f64,
    exit_price: f64,
    quantity: f64,
    entry_fee: f64,
    exit_fee: f64,
) -> (f64, f64) {
    let gross = (exit_price - entry) * quantity * side.direction();
    (gross, gross - entry_fee - exit_fee)
}

pub fn estimated_round_trip_cost_pct(bid: f64, ask: f64) -> f64 {
    if bid <= 0.0 || ask <= 0.0 {
        return f64::INFINITY;
    }
    let spread = (ask - bid) / ((ask + bid) / 2.0);
    let variable = spread + 2.0 * (SETTINGS.taker_fee_pct + SETTINGS.slippage_pct);
    variable * SETTINGS.cost_safety_multiplier
}

/// Returns the minimum target distance needed to satisfy the after-cost R/R
/// gate, while preserving the configured gross R target as a lower bound.
pub fn cost_aware_target_distance_pct(
    stop_distance_pct: f64,
    cost_pct: f64,
    base_target_r: f64,
    min_net_reward_risk: f64,
    max_target_pct: f64,
) -> Option<f64> {
    if !stop_distance_pct.is_finite()
        || !cost_pct.is_finite()
        || stop_distance_pct <= 0.0
        || cost_pct < 0.0
        || base_target_r <= 0.0
        || min_net_reward_risk <= 0.0
        || max_target_pct <= 0.0
    {
        return None;
    }

    let base_distance = stop_distance_pct * base_target_r;
    let after_cost_distance = cost_pct + min_net_reward_risk * (stop_distance_pct + cost_pct);
    let distance = base_distance.max(after_cost_distance);
    (distance.is_finite() && distance <= max_target_pct).then_some(distance)
}

pub fn net_reward_risk(target_distance_pct: f64, stop_distance_pct: f64, cost_pct: f64) -> f64 {
    let net_risk = stop_distance_pct + cost_pct;
    if net_risk <= 0.0 {
        return 0.0;
    }
    (target_distance_pct - cost_pct) / net_risk
}

/// Estimated net loss per unit when the structural stop is reached.
pub fn stop_loss_per_unit(side: Side, bid: f64, ask: f64, stop_price: f64) -> f64 {
    if bid <= 0.0 || ask <= 0.0 || stop_price <= 0.0 {
        return f64::INFINITY;
    }
    let entry = entry_fill(side, bid, ask, 1.0);
    let exit = exit_fill(side, stop_price, stop_price, 1.0);
    let (_, net) = trade_pnl(side, entry.price, exit.price, 1.0, entry.fee, exit.fee);
    (-net).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_worse_than_touch() {
        let long = entry_fill(Side::Long, 100.0, 100.1, 1.0);
        assert!(long.price > 100.1);
        let short = entry_fill(Side::Short, 100.0, 100.1, 1.0);
        assert!(short.price < 100.0);
    }

    #[test]
    fn exit_is_worse_than_touch() {
        let long = exit_fill(Side::Long, 100.0, 100.1, 1.0);
        assert!(long.price < 100.0);
        let short = exit_fill(Side::Short, 100.0, 100.1, 1.0);
        assert!(short.price > 100.1);
    }

    #[test]
    fn flat_round_trip_is_a_net_loss() {
        let entry = entry_fill(Side::Long, 100.0, 100.05, 2.0);
        let exit = exit_fill(Side::Long, 100.0, 100.05, 2.0);
        let (_, net) = trade_pnl(
            Side::Long,
            entry.price,
            exit.price,
            2.0,
            entry.fee,
            exit.fee,
        );
        assert!(net < 0.0, "crossing the spread twice must cost money");
    }

    #[test]
    fn round_trip_cost_includes_safety_margin() {
        let cost = estimated_round_trip_cost_pct(100.0, 100.05);
        let raw = 0.05 / 100.025 + 2.0 * (0.0005 + 0.0003);
        assert!(cost > raw, "safety multiplier must inflate the estimate");
    }

    #[test]
    fn cost_aware_target_satisfies_net_reward_risk() {
        let stop = 0.0027;
        let cost = 0.0022;
        let target = cost_aware_target_distance_pct(stop, cost, 1.8, 1.35, 0.01).unwrap();
        assert!((target - 0.008815).abs() < 1e-12);
        assert!(net_reward_risk(target, stop, cost) >= 1.35 - 1e-12);
    }

    #[test]
    fn zero_cost_keeps_the_base_target() {
        let target = cost_aware_target_distance_pct(0.003, 0.0, 1.8, 1.35, 0.01).unwrap();
        assert!((target - 0.0054).abs() < 1e-12);
    }

    #[test]
    fn infeasible_target_is_rejected() {
        assert!(cost_aware_target_distance_pct(0.003, 0.003, 1.8, 1.35, 0.005).is_none());
    }

    #[test]
    fn stop_loss_sizing_includes_costs() {
        let loss = stop_loss_per_unit(Side::Long, 100.0, 100.05, 99.5);
        assert!(
            loss > 0.55,
            "fees and slippage must increase loss beyond the raw stop distance"
        );
    }
}
