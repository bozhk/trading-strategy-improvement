use crate::config::SETTINGS;
use crate::models::{OrderBook, Side};

#[derive(Debug, Clone, Copy)]
pub struct BookFill {
    pub price: f64,
    pub quantity: f64,
    pub fee: f64,
    pub slippage_cost: f64,
}

/// Conservatively fills the whole requested quantity against the currently
/// visible L2 book. Returns None instead of assuming liquidity beyond depth 50.
pub fn book_fill(
    book: &OrderBook,
    side: Side,
    quantity: f64,
    is_entry: bool,
) -> Option<BookFill> {
    if !quantity.is_finite() || quantity <= 0.0 {
        return None;
    }
    let buy = if is_entry { side == Side::LONG } else { side == Side::SHORT };
    let levels: Vec<(f64, f64)> = if buy {
        book.asks.iter().map(|(price, size)| (price.0, *size)).collect()
    } else {
        book.bids
            .iter()
            .rev()
            .map(|(price, size)| (price.0, *size))
            .collect()
    };
    let touch = levels.first()?.0;
    let mut remaining = quantity;
    let mut notional = 0.0;
    for (price, available) in levels {
        let filled = remaining.min(available.max(0.0));
        notional += price * filled;
        remaining -= filled;
        if remaining <= quantity * 1e-12 {
            break;
        }
    }
    if remaining > quantity * 1e-12 {
        return None;
    }
    let price = notional / quantity;
    Some(BookFill {
        price,
        quantity,
        fee: notional * SETTINGS.taker_fee_pct,
        slippage_cost: (price - touch).abs() * quantity,
    })
}

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
        let (_, net) = trade_pnl(
            Side::LONG,
            entry.price,
            exit.price,
            2.0,
            entry.fee,
            exit.fee,
        );
        assert!(net < 0.0, "crossing the spread twice must cost money");
    }

    #[test]
    fn book_fill_uses_visible_depth_vwap() {
        let mut book = OrderBook::default();
        book.apply(
            "snapshot",
            &[(100.0, 2.0)],
            &[(100.1, 1.0), (100.2, 2.0)],
            1,
        );
        let fill = book_fill(&book, Side::LONG, 2.0, true).unwrap();
        assert!((fill.price - 100.15).abs() < 1e-9);
        assert!(fill.slippage_cost > 0.0);
    }

    #[test]
    fn book_fill_rejects_partial_visible_depth() {
        let mut book = OrderBook::default();
        book.apply("snapshot", &[(100.0, 1.0)], &[(100.1, 1.0)], 1);
        assert!(book_fill(&book, Side::LONG, 2.0, true).is_none());
    }

    #[test]
    fn round_trip_cost_includes_safety_margin() {
        let cost = estimated_round_trip_cost_pct(100.0, 100.05);
        let raw = 0.05 / 100.025 + 2.0 * (0.0005 + 0.0003);
        assert!(cost > raw, "safety multiplier must inflate the estimate");
    }
}
