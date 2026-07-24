use crate::config::SETTINGS;
use serde_json::{json, Value};

/// `pnls` is ordered newest-first (like the closed-trades deque).
pub fn summarize(pnls: &[f64]) -> Value {
    let count = pnls.len();
    let wins: Vec<f64> = pnls.iter().copied().filter(|v| *v > 0.0).collect();
    let losses: Vec<f64> = pnls.iter().copied().filter(|v| *v < 0.0).collect();
    let win_rate = if count > 0 {
        wins.len() as f64 / count as f64
    } else {
        0.0
    };

    // Wilson 95% interval: prevents tiny lucky samples from looking ready.
    let z = 1.96_f64;
    let (center, margin) = if count > 0 {
        let n = count as f64;
        let denom = 1.0 + z * z / n;
        let center = (win_rate + z * z / (2.0 * n)) / denom;
        let margin = z * ((win_rate * (1.0 - win_rate) + z * z / (4.0 * n)) / n).sqrt() / denom;
        (center, margin)
    } else {
        (0.0, 0.0)
    };

    let mut running = 0.0_f64;
    let mut peak = 0.0_f64;
    let mut max_drawdown = 0.0_f64;
    for value in pnls.iter().rev() {
        running += value;
        peak = peak.max(running);
        max_drawdown = max_drawdown.max(peak - running);
    }

    let gross_profit: f64 = wins.iter().sum();
    let gross_loss: f64 = losses.iter().sum::<f64>().abs();
    let profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else if !wins.is_empty() {
        f64::MAX
    } else {
        0.0
    };

    json!({
        "trades": count,
        "win_rate": win_rate * 100.0,
        "win_rate_ci95": [
            ((center - margin).max(0.0)) * 100.0,
            ((center + margin).min(1.0)) * 100.0,
        ],
        "expectancy": if count > 0 { pnls.iter().sum::<f64>() / count as f64 } else { 0.0 },
        "profit_factor": profit_factor,
        "max_drawdown": max_drawdown,
        "max_drawdown_pct": max_drawdown / SETTINGS.account_equity,
    })
}

pub fn live_readiness(pnls: &[f64], data_quality_errors: u64) -> Value {
    let report = summarize(pnls);
    let trades = report["trades"].as_u64().unwrap_or(0) as usize;
    let expectancy = report["expectancy"].as_f64().unwrap_or(0.0);
    let profit_factor = report["profit_factor"].as_f64().unwrap_or(0.0);
    let drawdown_pct = report["max_drawdown_pct"].as_f64().unwrap_or(1.0);

    let checks = json!({
        "sample_size": trades >= SETTINGS.min_live_trades,
        "net_expectancy": expectancy > SETTINGS.min_live_expectancy,
        "profit_factor": profit_factor >= SETTINGS.min_live_profit_factor,
        "drawdown": drawdown_pct <= SETTINGS.max_live_drawdown_pct,
        "data_quality": data_quality_errors == 0,
    });
    let passed = checks
        .as_object()
        .map(|m| m.values().all(|v| v.as_bool().unwrap_or(false)))
        .unwrap_or(false);

    // Deliberately stays locked: this binary has no broker/order endpoint.
    json!({
        "status": "NOT READY FOR LIVE",
        "live_enabled": false,
        "checks": checks,
        "metrics": report,
        "passed": passed,
        "notice": "Paper/replay evidence is not a guarantee of future results.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_lucky_sample_never_passes() {
        let pnls = vec![5.0; 10];
        let gate = live_readiness(&pnls, 0);
        assert_eq!(gate["live_enabled"], false);
        assert_eq!(gate["checks"]["sample_size"], false);
    }

    #[test]
    fn live_stays_disabled_even_when_all_checks_pass() {
        // 200 alternating trades with strongly positive expectancy.
        let mut pnls = Vec::new();
        for i in 0..200 {
            pnls.push(if i % 3 == 0 { -4.0 } else { 6.0 });
        }
        let gate = live_readiness(&pnls, 0);
        assert_eq!(gate["passed"], true);
        assert_eq!(gate["live_enabled"], false, "no order endpoint exists");
    }

    #[test]
    fn data_quality_errors_block_readiness() {
        let pnls = vec![6.0; 300];
        let gate = live_readiness(&pnls, 3);
        assert_eq!(gate["checks"]["data_quality"], false);
        assert_eq!(gate["passed"], false);
    }

    #[test]
    fn drawdown_is_computed_oldest_to_newest() {
        // Newest-first: latest trades recovered, older run had a deep hole.
        let pnls = vec![50.0, 50.0, -100.0, -100.0, 100.0];
        let report = summarize(&pnls);
        assert!(report["max_drawdown"].as_f64().unwrap() >= 200.0);
    }
}
