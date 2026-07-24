use parking_lot::Mutex;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;

const TELEGRAM_MESSAGE_LIMIT: usize = 4096;

#[derive(Debug, Clone)]
struct TelegramConfig {
    token: String,
    chat_id: String,
    thread_id: Option<i64>,
    rejection_summary_seconds: u64,
    notify_rejections: bool,
    notify_startup: bool,
}

impl TelegramConfig {
    fn from_env() -> Option<Self> {
        let token = env::var("TELEGRAM_BOT_TOKEN").ok()?.trim().to_string();
        let chat_id = env::var("TELEGRAM_CHAT_ID").ok()?.trim().to_string();
        if token.is_empty() || chat_id.is_empty() {
            return None;
        }

        Some(Self {
            token,
            chat_id,
            thread_id: env::var("TELEGRAM_THREAD_ID")
                .ok()
                .and_then(|value| value.trim().parse().ok()),
            rejection_summary_seconds: env_u64("TELEGRAM_REJECT_SUMMARY_SECONDS", 300).max(30),
            notify_rejections: env_bool("TELEGRAM_NOTIFY_REJECTIONS", true),
            notify_startup: env_bool("TELEGRAM_NOTIFY_STARTUP", true),
        })
    }
}

#[derive(Debug)]
struct Notifier {
    immediate_tx: mpsc::UnboundedSender<String>,
    rejections: Arc<Mutex<HashMap<(String, String), u64>>>,
    notify_rejections: bool,
}

static NOTIFIER: OnceLock<Notifier> = OnceLock::new();

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .map(|value| matches!(value.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

pub fn start(trading_mode: &str) {
    let Some(config) = TelegramConfig::from_env() else {
        tracing::info!(
            "Telegram notifications disabled: TELEGRAM_BOT_TOKEN or TELEGRAM_CHAT_ID is missing"
        );
        return;
    };

    let (immediate_tx, immediate_rx) = mpsc::unbounded_channel();
    let rejections = Arc::new(Mutex::new(HashMap::new()));
    let notifier = Notifier {
        immediate_tx,
        rejections: rejections.clone(),
        notify_rejections: config.notify_rejections,
    };
    if NOTIFIER.set(notifier).is_err() {
        tracing::warn!("Telegram notifier was already started");
        return;
    }

    let mode = trading_mode.to_uppercase();
    tokio::spawn(worker(config, immediate_rx, rejections, mode));
    tracing::info!("Telegram notifications enabled");
}

pub fn notify_entry(event: EntryNotification<'_>) {
    let text = format!(
        "ОТКРЫТА PAPER-СДЕЛКА\n\n{} {}\nВход: {:.6}\nКоличество: {:.6}\nСтоп: {:.6}\nЦель: {:.6}\nРиск: {:.2} USDT\nNet R/R: {:.2}\nЦель: {:.2}R",
        event.symbol,
        event.side,
        event.entry,
        event.quantity,
        event.stop,
        event.target,
        event.risk_budget,
        event.net_rr,
        event.target_r,
    );
    send_immediate(text);
}

pub fn notify_exit(event: ExitNotification<'_>) {
    let result = if event.pnl > 0.0 {
        "ПРИБЫЛЬ"
    } else if event.pnl < 0.0 {
        "УБЫТОК"
    } else {
        "БЕЗУБЫТОК"
    };
    let text = format!(
        "ЗАКРЫТА PAPER-СДЕЛКА [{result}]\n\n{} {}\nПричина: {}\nВход: {:.6}\nВыход: {:.6}\nNet PnL: {:+.2} USDT\nGross PnL: {:+.2} USDT\nКомиссии: {:.2} USDT\nУдержание: {:.0} сек\nMFE / MAE: {:+.2} / {:+.2} USDT",
        event.symbol,
        event.side,
        event.reason,
        event.entry,
        event.exit,
        event.pnl,
        event.gross_pnl,
        event.fees,
        event.held_seconds,
        event.mfe,
        event.mae,
    );
    send_immediate(text);
}

pub fn notify_rejection(symbol: &str, reason: &str) {
    let Some(notifier) = NOTIFIER.get() else {
        return;
    };
    if !notifier.notify_rejections {
        return;
    }
    let key = (truncate(symbol, 32), truncate(reason, 180));
    *notifier.rejections.lock().entry(key).or_insert(0) += 1;
}

fn send_immediate(text: String) {
    if let Some(notifier) = NOTIFIER.get() {
        let _ = notifier.immediate_tx.send(text);
    }
}

async fn worker(
    config: TelegramConfig,
    mut immediate_rx: mpsc::UnboundedReceiver<String>,
    rejections: Arc<Mutex<HashMap<(String, String), u64>>>,
    mode: String,
) {
    let client = match Client::builder().timeout(Duration::from_secs(10)).build() {
        Ok(client) => client,
        Err(_) => {
            tracing::error!("Telegram HTTP client initialization failed");
            return;
        }
    };
    let mut interval = tokio::time::interval(Duration::from_secs(config.rejection_summary_seconds));
    interval.tick().await;

    if config.notify_startup {
        let text = format!(
            "PulseBook запущен\n\nРежим: {mode}\nLive trading: ЗАБЛОКИРОВАН\nУведомления об открытиях, закрытиях и отклонениях активны."
        );
        send_message(&client, &config, &text).await;
    }

    loop {
        tokio::select! {
            message = immediate_rx.recv() => {
                let Some(message) = message else {
                    flush_rejections(&client, &config, &rejections).await;
                    break;
                };
                send_message(&client, &config, &message).await;
            }
            _ = interval.tick(), if config.notify_rejections => {
                flush_rejections(&client, &config, &rejections).await;
            }
        }
    }
}

async fn flush_rejections(
    client: &Client,
    config: &TelegramConfig,
    rejections: &Mutex<HashMap<(String, String), u64>>,
) {
    let mut rows: Vec<((String, String), u64)> = {
        let mut pending = rejections.lock();
        pending.drain().collect()
    };
    if rows.is_empty() {
        return;
    }

    rows.sort_by_key(|item| std::cmp::Reverse(item.1));
    let total: u64 = rows.iter().map(|(_, count)| count).sum();
    let mut text = format!(
        "СВОДКА ОТКЛОНЕНИЙ СДЕЛОК\n\nПериод: {} сек\nВсего: {total}\n",
        config.rejection_summary_seconds
    );
    for ((symbol, reason), count) in rows.iter().take(20) {
        let line = format!("\n{symbol}: {reason} × {count}");
        if text.len() + line.len() > TELEGRAM_MESSAGE_LIMIT - 80 {
            break;
        }
        text.push_str(&line);
    }
    if rows.len() > 20 {
        text.push_str(&format!("\n\nИ ещё {} групп причин.", rows.len() - 20));
    }
    send_message(client, config, &text).await;
}

async fn send_message(client: &Client, config: &TelegramConfig, text: &str) {
    let endpoint = format!("https://api.telegram.org/bot{}/sendMessage", config.token);
    let mut payload = json!({
        "chat_id": config.chat_id,
        "text": truncate(text, TELEGRAM_MESSAGE_LIMIT),
        "disable_web_page_preview": true,
    });
    if let Some(thread_id) = config.thread_id {
        payload["message_thread_id"] = json!(thread_id);
    }

    let response = match client.post(endpoint).json(&payload).send().await {
        Ok(response) => response,
        Err(_) => {
            // Do not print the reqwest error: it may contain the bot token URL.
            tracing::warn!("Telegram request failed; trading continues normally");
            return;
        }
    };
    if response.status().is_success() {
        return;
    }

    let status = response.status();
    let description = response
        .json::<Value>()
        .await
        .ok()
        .and_then(|body| body["description"].as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown Telegram API error".to_string());
    tracing::warn!(%status, %description, "Telegram rejected a notification");
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

pub struct EntryNotification<'a> {
    pub symbol: &'a str,
    pub side: &'a str,
    pub entry: f64,
    pub quantity: f64,
    pub stop: f64,
    pub target: f64,
    pub risk_budget: f64,
    pub net_rr: f64,
    pub target_r: f64,
}

pub struct ExitNotification<'a> {
    pub symbol: &'a str,
    pub side: &'a str,
    pub reason: &'a str,
    pub entry: f64,
    pub exit: f64,
    pub pnl: f64,
    pub gross_pnl: f64,
    pub fees: f64,
    pub held_seconds: f64,
    pub mfe: f64,
    pub mae: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_truncation_keeps_valid_utf8() {
        assert_eq!(truncate("тест", 5), "те");
    }

    #[test]
    fn short_text_is_not_changed() {
        assert_eq!(truncate("BTCUSDT", 32), "BTCUSDT");
    }
}
