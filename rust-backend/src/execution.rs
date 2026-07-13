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
    let touch = if side == Side::LONG { ask } else { bid };
    let price = touch * (1.0 + side.direction() * SETTINGS.slippage_pct);
    Fill {
        price,
        fee: price * quantity * SETTINGS.taker_fee_pct,
        slippage_cost: (price - touch).abs() * quantity,
    }
}

/// Liquidating exit crosses to the opposite side of the spread.
pub fn exit_fill(side: Side, bid: f64, ask: f64, quantity: f64) -> Fill {
    let touch = if side == Side::LONG { bid } else { ask };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_worse_than_touch() {
        let long = entry_fill(Side::LONG, 100.0, 100.1, 1.0);
        assert!(long.price > 100.1);
        let short = entry_fill(Side::SHORT, 100.0, 100.1, 1.0);
        assert!(short.price < 100.0);
    }

    #[test]
    fn exit_is_worse_than_touch() {
        let long = exit_fill(Side::LONG, 100.0, 100.1, 1.0);
        assert!(long.price < 100.0);
        let short = exit_fill(Side::SHORT, 100.0, 100.1, 1.0);
        assert!(short.price > 100.1);
    }

    #[test]
    fn flat_round_trip_is_a_net_loss() {
        let entry = entry_fill(Side::LONG, 100.0, 100.05, 2.0);
        let exit = exit_fill(Side::LONG, 100.0, 100.05, 2.0);
        let (_, net) = trade_pnl(Side::LONG, entry.price, exit.price, 2.0, entry.fee, exit.fee);
        assert!(net < 0.0, "crossing the spread twice must cost money");
    }

    #[test]
    fn round_trip_cost_includes_safety_margin() {
        let cost = estimated_round_trip_cost_pct(100.0, 100.05);
        let raw = 0.05 / 100.025 + 2.0 * (0.0005 + 0.0003);
        assert!(cost > raw, "safety multiplier must inflate the estimate");
    }
}
