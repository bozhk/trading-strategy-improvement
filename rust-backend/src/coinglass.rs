use axum::{extract::Query, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{env, sync::LazyLock, time::Duration};

const HEATMAP_URL: &str =
    "https://open-api-v4.coinglass.com/api/futures/liquidation/heatmap/model1";
const ALLOWED_RANGES: &[&str] = &["12h", "24h", "3d", "7d", "30d", "90d", "180d", "1y"];

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("build Coinglass HTTP client")
});

#[derive(Debug, Deserialize)]
pub struct HeatmapQuery {
    symbol: String,
    #[serde(default = "default_range")]
    range: String,
}

fn default_range() -> String {
    "3d".to_string()
}

type ApiError = (StatusCode, Json<Value>);

fn error(status: StatusCode, message: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": message.into() })))
}

fn validate_query(query: &HeatmapQuery) -> Result<String, ApiError> {
    let symbol = query.symbol.trim().to_uppercase();
    if symbol.is_empty()
        || symbol.len() > 24
        || !symbol
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(error(StatusCode::BAD_REQUEST, "Некорректный символ монеты"));
    }
    if !ALLOWED_RANGES.contains(&query.range.as_str()) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Неподдерживаемый диапазон heatmap",
        ));
    }
    if !crate::state::STATE.lock().books.contains_key(&symbol) {
        return Err(error(
            StatusCode::NOT_FOUND,
            "Монета отсутствует в активной группе PulseBook",
        ));
    }
    Ok(symbol)
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
        .filter(|value: &f64| value.is_finite())
}

fn largest_liquidations(data: &Value) -> Vec<Value> {
    let y_axis = data["y_axis"].as_array();
    let candles = data["price_candlesticks"].as_array();
    let current_price = candles
        .and_then(|rows| rows.last())
        .and_then(Value::as_array)
        .and_then(|row| row.get(4))
        .and_then(number)
        .unwrap_or_default();
    let mut clusters = data["liquidation_leverage_data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|point| {
            let point = point.as_array()?;
            let x = point.first()?.as_u64()? as usize;
            let y = point.get(1)?.as_u64()? as usize;
            let amount = number(point.get(2)?)?;
            let price = y_axis.and_then(|levels| levels.get(y)).and_then(number)?;
            let timestamp = candles
                .and_then(|rows| rows.get(x))
                .and_then(Value::as_array)
                .and_then(|row| row.first())
                .and_then(Value::as_u64);
            Some((amount, x, y, price, timestamp))
        })
        .collect::<Vec<_>>();
    clusters.sort_by(|left, right| right.0.total_cmp(&left.0));
    clusters
        .into_iter()
        .take(12)
        .map(|(amount, x, y, price, timestamp)| {
            json!({
                "amount": amount,
                "price": price,
                "x_index": x,
                "y_index": y,
                "timestamp": timestamp,
                "side": if current_price > 0.0 && price < current_price { "LONG" } else { "SHORT" },
            })
        })
        .collect()
}

pub async fn liquidation_heatmap(
    Query(query): Query<HeatmapQuery>,
) -> Result<Json<Value>, ApiError> {
    let symbol = validate_query(&query)?;
    let api_key = env::var("COINGLASS_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "COINGLASS_API_KEY не настроен на сервере",
            )
        })?;

    let response = CLIENT
        .get(HEATMAP_URL)
        .header("accept", "application/json")
        .header("CG-API-KEY", api_key)
        .query(&[
            ("exchange", "Binance"),
            ("symbol", symbol.as_str()),
            ("range", query.range.as_str()),
        ])
        .send()
        .await
        .map_err(|err| {
            error(
                StatusCode::BAD_GATEWAY,
                format!("Coinglass недоступен: {err}"),
            )
        })?;
    let status = response.status();
    let payload: Value = response.json().await.map_err(|err| {
        error(
            StatusCode::BAD_GATEWAY,
            format!("Некорректный ответ Coinglass: {err}"),
        )
    })?;

    if !status.is_success() || payload["code"].as_str().is_some_and(|code| code != "0") {
        let message = payload["msg"]
            .as_str()
            .unwrap_or("Coinglass отклонил запрос. Проверьте API-ключ, тариф и лимиты.");
        return Err(error(StatusCode::BAD_GATEWAY, message));
    }
    let data = &payload["data"];
    if !data["y_axis"].is_array()
        || !data["liquidation_leverage_data"].is_array()
        || !data["price_candlesticks"].is_array()
    {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Coinglass вернул неполные данные heatmap",
        ));
    }

    Ok(Json(json!({
        "source": "Coinglass",
        "exchange": "Binance",
        "symbol": symbol,
        "range": query.range,
        "fetched_at": crate::models::now_ts(),
        "y_axis": data["y_axis"],
        "liquidation_leverage_data": data["liquidation_leverage_data"],
        "price_candlesticks": data["price_candlesticks"],
        "largest_liquidations": largest_liquidations(data),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_largest_clusters_with_side() {
        let data = json!({
            "y_axis": [90, 110],
            "liquidation_leverage_data": [[0, 0, 10], [1, 1, 50]],
            "price_candlesticks": [[1, "99", "101", "98", "100", "1"], [2, "100", "102", "99", "100", "1"]]
        });
        let clusters = largest_liquidations(&data);
        assert_eq!(clusters[0]["amount"].as_f64(), Some(50.0));
        assert_eq!(clusters[0]["side"], "SHORT");
        assert_eq!(clusters[1]["side"], "LONG");
    }
}
