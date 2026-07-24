use crate::config::SETTINGS;
use crate::models::{now_ts, Instrument};
use crate::state::STATE;
use crate::stream::StreamManager;
use serde_json::{json, Value};
use std::time::Duration;

fn eligible(price: f64, turnover: f64, spread: f64) -> bool {
    price >= SETTINGS.min_price
        && turnover > SETTINGS.min_turnover
        && spread <= SETTINGS.max_spread_pct
}

async fn fetch_json(
    client: &reqwest::Client,
    path: &str,
    params: &[(&str, &str)],
) -> Result<Value, String> {
    let url = format!("{}{}", SETTINGS.rest_url, path);
    let response = client
        .get(&url)
        .query(params)
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let payload: Value = response.json().await.map_err(|e| e.to_string())?;
    if payload["retCode"].as_i64().unwrap_or(-1) != 0 {
        return Err(payload["retMsg"]
            .as_str()
            .unwrap_or("Bybit error")
            .to_string());
    }
    Ok(payload["result"].clone())
}

pub async fn scan_live() -> Result<Vec<Instrument>, String> {
    let client = reqwest::Client::builder()
        .user_agent("PulseBook-Simulator/1.0 (rust)")
        .build()
        .map_err(|e| e.to_string())?;

    let (info, tickers) = tokio::try_join!(
        fetch_json(
            &client,
            "/v5/market/instruments-info",
            &[("category", "linear"), ("limit", "1000")]
        ),
        fetch_json(&client, "/v5/market/tickers", &[("category", "linear")]),
    )?;

    let active: std::collections::HashSet<&str> = info["list"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|x| {
                    x["status"].as_str() == Some("Trading")
                        && x["quoteCoin"].as_str() == Some("USDT")
                        && x["contractType"].as_str() == Some("LinearPerpetual")
                })
                .filter_map(|x| x["symbol"].as_str())
                .collect()
        })
        .unwrap_or_default();

    let parse = |v: &Value| -> f64 { v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0) };
    let mut candidates: Vec<Instrument> = Vec::new();
    for t in tickers["list"].as_array().unwrap_or(&Vec::new()) {
        let Some(symbol) = t["symbol"].as_str() else {
            continue;
        };
        if !active.contains(symbol) {
            continue;
        }
        let price = parse(&t["lastPrice"]);
        let turnover = parse(&t["turnover24h"]);
        let bid = parse(&t["bid1Price"]);
        let ask = parse(&t["ask1Price"]);
        let spread = if bid > 0.0 && ask > 0.0 {
            (ask - bid) / bid * 100.0
        } else {
            999.0
        };
        if eligible(price, turnover, spread) {
            candidates.push(Instrument {
                symbol: symbol.to_string(),
                price,
                turnover24h: turnover,
                spread_pct: spread,
            });
        }
    }
    candidates.sort_by(|a, b| b.turnover24h.total_cmp(&a.turnover24h));
    candidates.truncate(SETTINGS.max_symbols);
    Ok(candidates)
}

pub async fn radar_loop(manager: &mut StreamManager) {
    loop {
        match scan_live().await {
            Ok(rows) if !rows.is_empty() => {
                {
                    let mut state = STATE.lock();
                    state.radar = rows
                        .iter()
                        .map(|x| {
                            json!({
                                "symbol": x.symbol, "price": x.price,
                                "turnover24h": x.turnover24h, "spread_pct": x.spread_pct,
                            })
                        })
                        .collect();
                    state.last_scan = now_ts();
                    let message = format!("Radar selected {} liquid perpetuals", rows.len());
                    state.log("SCAN", &message, "SYSTEM", 0.0);
                }
                let symbols: Vec<String> = rows.iter().map(|x| x.symbol.clone()).collect();
                manager.set_symbols(symbols, false).await;
            }
            other => {
                let error = match other {
                    Err(e) => e,
                    _ => "no instruments passed liquidity filters".to_string(),
                };
                let message = format!("Live radar unavailable: {error:.80}");
                STATE.lock().log("WARN", &message, "SYSTEM", 30.0);
                if SETTINGS.demo_fallback {
                    let symbols: Vec<String> = SETTINGS
                        .demo_symbols
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    manager.set_symbols(symbols, true).await;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(SETTINGS.scan_interval)).await;
    }
}
