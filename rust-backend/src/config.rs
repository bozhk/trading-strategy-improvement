use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

fn env_str(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_f64(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

const ADMIN_SETTING_KEYS: &[&str] = &[
    "force_demo",
    "invert_sides",
    "demo_fallback",
    "scan_interval",
    "max_symbols",
    "min_turnover",
    "min_price",
    "max_spread_pct",
    "ws_chunk_size",
    "wall_multiplier",
    "demo_symbols",
    "trading_mode",
    "strategy_mode",
    "account_equity",
    "risk_per_trade_pct",
    "max_position_notional",
    "max_open_positions",
    "max_daily_loss_pct",
    "max_consecutive_losses",
    "cooldown_seconds",
    "loss_cooldown_seconds",
    "taker_fee_pct",
    "slippage_pct",
    "cost_safety_multiplier",
    "max_holding_seconds",
    "trail_arm_r",
    "trail_giveback_r",
    "breakeven_arm_r",
    "reversal_confirm_ticks",
    "reversal_confirm_seconds",
    "reversal_min_hold_seconds",
    "alternative_wall_multiplier",
    "alternative_signal_expiry_seconds",
    "alternative_retest_tolerance_pct",
    "alternative_retest_hold_ticks",
    "alternative_min_confluence_score",
    "alternative_min_net_reward_risk",
    "alternative_target_r_multiple",
    "alternative_max_target_pct",
    "alternative_min_stop_pct",
    "alternative_max_stop_pct",
    "alternative_structure_buffer_pct",
    "fvg_candle_seconds",
    "fvg_min_gap_pct",
    "fvg_signal_expiry_seconds",
    "fvg_entry_depth",
    "fvg_min_confluence_score",
    "fvg_min_net_reward_risk",
    "fvg_target_r_multiple",
    "fvg_max_target_pct",
    "fvg_min_stop_pct",
    "fvg_max_stop_pct",
    "fvg_stop_buffer_pct",
    "min_live_trades",
    "min_live_profit_factor",
    "min_live_expectancy",
    "max_live_drawdown_pct",
];

fn env_file_path() -> String {
    env_str("PULSEBOOK_ENV_FILE", "pulsebook.env")
}

fn load_env_file() {
    let path = env_file_path();
    let Ok(contents) = fs::read_to_string(&path) else {
        return;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        env::set_var(key, value.trim().trim_matches(['"', '\'']));
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub rest_url: String,
    pub ws_url: String,
    pub host: String,
    pub port: u16,
    pub force_demo: bool,
    pub invert_sides: bool,
    pub demo_fallback: bool,
    pub trading_mode: String,
    pub strategy_mode: String,
    pub scan_interval: u64,
    pub max_symbols: usize,
    pub min_turnover: f64,
    pub min_price: f64,
    pub max_spread_pct: f64,
    pub ws_chunk_size: usize,
    // Cost-aware execution.
    pub taker_fee_pct: f64,
    pub slippage_pct: f64,
    pub cost_safety_multiplier: f64,

    pub max_holding_seconds: f64,
    pub trail_arm_r: f64,
    pub trail_giveback_r: f64,
    pub breakeven_arm_r: f64,
    pub reversal_confirm_ticks: u32,
    pub reversal_confirm_seconds: f64,
    pub reversal_min_hold_seconds: f64,

    // Portfolio circuit breakers.
    pub account_equity: f64,
    pub risk_per_trade_pct: f64,
    pub max_position_notional: f64,
    pub max_open_positions: usize,
    pub max_daily_loss_pct: f64,
    pub max_consecutive_losses: usize,
    pub cooldown_seconds: f64,
    pub loss_cooldown_seconds: f64,

    // Alternative: absorbed wall -> breakout -> held retest.
    pub alternative_wall_multiplier: f64,
    pub alternative_signal_expiry_seconds: f64,
    pub alternative_retest_tolerance_pct: f64,
    pub alternative_retest_hold_ticks: u32,
    pub alternative_min_confluence_score: i64,
    pub alternative_min_net_reward_risk: f64,
    pub alternative_target_r_multiple: f64,
    pub alternative_max_target_pct: f64,
    pub alternative_min_stop_pct: f64,
    pub alternative_max_stop_pct: f64,
    pub alternative_structure_buffer_pct: f64,

    // FVG: three-candle imbalance followed by a return to the gap.
    pub fvg_candle_seconds: f64,
    pub fvg_min_gap_pct: f64,
    pub fvg_signal_expiry_seconds: f64,
    pub fvg_entry_depth: f64,
    pub fvg_min_confluence_score: i64,
    pub fvg_min_net_reward_risk: f64,
    pub fvg_target_r_multiple: f64,
    pub fvg_max_target_pct: f64,
    pub fvg_min_stop_pct: f64,
    pub fvg_max_stop_pct: f64,
    pub fvg_stop_buffer_pct: f64,

    // Readiness gate.
    pub min_live_trades: usize,
    pub min_live_profit_factor: f64,
    pub min_live_expectancy: f64,
    pub max_live_drawdown_pct: f64,
    pub demo_symbols: Vec<&'static str>,
}

impl Settings {
    fn new() -> Self {
        Self {
            rest_url: "https://api.bybit.com".into(),
            ws_url: "wss://stream.bybit.com/v5/public/linear".into(),
            host: env_str("HOST", "0.0.0.0"),
            port: env_u64("PORT", 8000) as u16,
            force_demo: env_bool("FORCE_DEMO", false)
                || env_str("DATA_MODE", "real").eq_ignore_ascii_case("demo"),
            invert_sides: env_bool("INVERT_SIDES", false),
            demo_fallback: env_bool("DEMO_FALLBACK", true),
            trading_mode: env_str("TRADING_MODE", "paper").to_lowercase(),
            strategy_mode: env_str("STRATEGY_MODE", "alternative").to_lowercase(),
            scan_interval: env_u64("SCAN_INTERVAL", 3600),
            max_symbols: env_u64("MAX_SYMBOLS", 40) as usize,
            min_turnover: 10_000_000.0,
            min_price: 0.005,
            max_spread_pct: 0.10,
            ws_chunk_size: 20,
            taker_fee_pct: env_f64("TAKER_FEE_PCT", 0.00050),
            slippage_pct: env_f64("SLIPPAGE_PCT", 0.00030),
            cost_safety_multiplier: 1.35,
            max_holding_seconds: env_f64("MAX_HOLDING_SECONDS", 900.0),
            trail_arm_r: 1.0,
            trail_giveback_r: 0.55,
            breakeven_arm_r: 0.8,
            reversal_confirm_ticks: env_u64("REVERSAL_CONFIRM_TICKS", 3) as u32,
            reversal_confirm_seconds: env_f64("REVERSAL_CONFIRM_SECONDS", 3.0),
            reversal_min_hold_seconds: env_f64("REVERSAL_MIN_HOLD_SECONDS", 30.0),
            account_equity: env_f64("ACCOUNT_EQUITY", env_f64("PAPER_EQUITY", 10_000.0)),
            risk_per_trade_pct: env_f64("RISK_PER_TRADE_PCT", 0.0025),
            max_position_notional: 1_000.0,
            max_open_positions: 2,
            max_daily_loss_pct: 0.015,
            max_consecutive_losses: 4,
            cooldown_seconds: 180.0,
            loss_cooldown_seconds: 900.0,
            alternative_wall_multiplier: env_f64("ALTERNATIVE_WALL_MULTIPLIER", 15.0),
            alternative_signal_expiry_seconds: env_f64("ALTERNATIVE_SIGNAL_EXPIRY_SECONDS", 8.0),
            alternative_retest_tolerance_pct: env_f64("ALTERNATIVE_RETEST_TOLERANCE_PCT", 0.0008),
            alternative_retest_hold_ticks: env_u64("ALTERNATIVE_RETEST_HOLD_TICKS", 3) as u32,
            alternative_min_confluence_score: env_u64("ALTERNATIVE_MIN_CONFLUENCE_SCORE", 85)
                as i64,
            alternative_min_net_reward_risk: env_f64("ALTERNATIVE_MIN_NET_REWARD_RISK", 1.35),
            alternative_target_r_multiple: env_f64("ALTERNATIVE_TARGET_R_MULTIPLE", 1.8),
            alternative_max_target_pct: env_f64("ALTERNATIVE_MAX_TARGET_PCT", 0.0100),
            alternative_min_stop_pct: env_f64("ALTERNATIVE_MIN_STOP_PCT", 0.0020),
            alternative_max_stop_pct: env_f64("ALTERNATIVE_MAX_STOP_PCT", 0.0075),
            alternative_structure_buffer_pct: env_f64("ALTERNATIVE_STRUCTURE_BUFFER_PCT", 0.0008),
            fvg_candle_seconds: env_f64("FVG_CANDLE_SECONDS", 5.0),
            fvg_min_gap_pct: env_f64("FVG_MIN_GAP_PCT", 0.0008),
            fvg_signal_expiry_seconds: env_f64("FVG_SIGNAL_EXPIRY_SECONDS", 120.0),
            fvg_entry_depth: env_f64("FVG_ENTRY_DEPTH", 0.5),
            fvg_min_confluence_score: env_u64("FVG_MIN_CONFLUENCE_SCORE", 60) as i64,
            fvg_min_net_reward_risk: env_f64("FVG_MIN_NET_REWARD_RISK", 1.35),
            fvg_target_r_multiple: env_f64("FVG_TARGET_R_MULTIPLE", 1.8),
            fvg_max_target_pct: env_f64("FVG_MAX_TARGET_PCT", 0.0100),
            fvg_min_stop_pct: env_f64("FVG_MIN_STOP_PCT", 0.0020),
            fvg_max_stop_pct: env_f64("FVG_MAX_STOP_PCT", 0.0075),
            fvg_stop_buffer_pct: env_f64("FVG_STOP_BUFFER_PCT", 0.0008),
            min_live_trades: 200,
            min_live_profit_factor: 1.20,
            min_live_expectancy: 0.0,
            max_live_drawdown_pct: 0.10,
            demo_symbols: vec![
                "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "DOGEUSDT", "LINKUSDT", "AVAXUSDT",
                "SUIUSDT",
            ],
        }
    }

    /// Live trading is hard-locked: the binary refuses to start in any
    /// mode that could imply real order routing.
    pub fn assert_safe_mode(&self) {
        if !matches!(self.trading_mode.as_str(), "paper" | "replay" | "simulator") {
            panic!("LIVE TRADING IS LOCKED: only paper/replay/simulator modes are allowed");
        }
        assert!(
            matches!(self.strategy_mode.as_str(), "alternative" | "fvg"),
            "STRATEGY_MODE must be alternative or fvg"
        );
        assert!(
            self.alternative_target_r_multiple > self.alternative_min_net_reward_risk
                && self.alternative_min_net_reward_risk > 0.0,
            "ALTERNATIVE_TARGET_R_MULTIPLE must be greater than ALTERNATIVE_MIN_NET_REWARD_RISK > 0"
        );
        assert!(
            self.fvg_target_r_multiple > self.fvg_min_net_reward_risk
                && self.fvg_min_net_reward_risk > 0.0,
            "FVG_TARGET_R_MULTIPLE must be greater than FVG_MIN_NET_REWARD_RISK > 0"
        );
        assert!(
            self.alternative_max_target_pct > 0.0 && self.fvg_max_target_pct > 0.0,
            "Strategy max target settings must be positive"
        );
        assert!(
            self.fvg_candle_seconds > 0.0
                && self.fvg_min_gap_pct > 0.0
                && self.fvg_signal_expiry_seconds > 0.0
                && (0.0..=1.0).contains(&self.fvg_entry_depth),
            "FVG settings are invalid"
        );
        assert!(
            self.reversal_confirm_ticks > 0
                && self.reversal_confirm_seconds > 0.0
                && self.reversal_min_hold_seconds > 0.0,
            "Reversal confirmation settings must be positive"
        );
        assert!(
            self.max_holding_seconds > 0.0,
            "MAX_HOLDING_SECONDS must be positive"
        );
        assert!(
            self.account_equity > 0.0 && self.risk_per_trade_pct > 0.0,
            "ACCOUNT_EQUITY and RISK_PER_TRADE_PCT must be positive"
        );
    }

    pub fn admin_password_configured(&self) -> bool {
        env::var("ADMIN_PASSWORD")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    }

    pub fn admin_password_matches(&self, password: &str) -> bool {
        env::var("ADMIN_PASSWORD")
            .map(|expected| !expected.is_empty() && expected == password)
            .unwrap_or(false)
    }

    pub fn public_settings(&self) -> Value {
        json!({
            "force_demo": self.force_demo,
            "invert_sides": self.invert_sides,
            "demo_fallback": self.demo_fallback,
            "trading_mode": self.trading_mode,
            "strategy_mode": self.strategy_mode,
            "scan_interval": self.scan_interval,
            "max_symbols": self.max_symbols,
            "min_turnover": self.min_turnover,
            "min_price": self.min_price,
            "max_spread_pct": self.max_spread_pct,
            "ws_chunk_size": self.ws_chunk_size,
            "demo_symbols": self.demo_symbols,
            "account_equity": self.account_equity,
            "risk_per_trade_pct": self.risk_per_trade_pct,
            "max_position_notional": self.max_position_notional,
            "max_open_positions": self.max_open_positions,
            "max_daily_loss_pct": self.max_daily_loss_pct,
            "max_consecutive_losses": self.max_consecutive_losses,
            "cooldown_seconds": self.cooldown_seconds,
            "loss_cooldown_seconds": self.loss_cooldown_seconds,
            "taker_fee_pct": self.taker_fee_pct,
            "slippage_pct": self.slippage_pct,
            "cost_safety_multiplier": self.cost_safety_multiplier,
            "max_holding_seconds": self.max_holding_seconds,
            "trail_arm_r": self.trail_arm_r,
            "trail_giveback_r": self.trail_giveback_r,
            "breakeven_arm_r": self.breakeven_arm_r,
            "reversal_confirm_ticks": self.reversal_confirm_ticks,
            "reversal_confirm_seconds": self.reversal_confirm_seconds,
            "reversal_min_hold_seconds": self.reversal_min_hold_seconds,
            "alternative_wall_multiplier": self.alternative_wall_multiplier,
            "alternative_signal_expiry_seconds": self.alternative_signal_expiry_seconds,
            "alternative_retest_tolerance_pct": self.alternative_retest_tolerance_pct,
            "alternative_retest_hold_ticks": self.alternative_retest_hold_ticks,
            "alternative_min_confluence_score": self.alternative_min_confluence_score,
            "alternative_min_net_reward_risk": self.alternative_min_net_reward_risk,
            "alternative_target_r_multiple": self.alternative_target_r_multiple,
            "alternative_max_target_pct": self.alternative_max_target_pct,
            "alternative_min_stop_pct": self.alternative_min_stop_pct,
            "alternative_max_stop_pct": self.alternative_max_stop_pct,
            "alternative_structure_buffer_pct": self.alternative_structure_buffer_pct,
            "fvg_candle_seconds": self.fvg_candle_seconds,
            "fvg_min_gap_pct": self.fvg_min_gap_pct,
            "fvg_signal_expiry_seconds": self.fvg_signal_expiry_seconds,
            "fvg_entry_depth": self.fvg_entry_depth,
            "fvg_min_confluence_score": self.fvg_min_confluence_score,
            "fvg_min_net_reward_risk": self.fvg_min_net_reward_risk,
            "fvg_target_r_multiple": self.fvg_target_r_multiple,
            "fvg_max_target_pct": self.fvg_max_target_pct,
            "fvg_min_stop_pct": self.fvg_min_stop_pct,
            "fvg_max_stop_pct": self.fvg_max_stop_pct,
            "fvg_stop_buffer_pct": self.fvg_stop_buffer_pct,
            "min_live_trades": self.min_live_trades,
            "min_live_profit_factor": self.min_live_profit_factor,
            "min_live_expectancy": self.min_live_expectancy,
            "max_live_drawdown_pct": self.max_live_drawdown_pct,
        })
    }

    pub fn save_admin_settings(&self, values: &Value) -> Result<(), String> {
        let object = values
            .as_object()
            .ok_or_else(|| "Settings payload must be a JSON object".to_string())?;
        for key in object.keys() {
            if !ADMIN_SETTING_KEYS.contains(&key.as_str()) {
                return Err(format!("Unsupported setting: {key}"));
            }
        }
        validate_admin_settings(object)?;

        let path = env_file_path();
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
        for key in ADMIN_SETTING_KEYS {
            let Some(value) = object.get(*key) else {
                continue;
            };
            let env_key = key.to_uppercase();
            let encoded = env_value(value)?;
            let replacement = format!("{env_key}={encoded}");
            if let Some(line) = lines.iter_mut().find(|line| {
                line.split_once('=').map(|(name, _)| name.trim()) == Some(env_key.as_str())
            }) {
                *line = replacement;
            } else {
                lines.push(replacement);
            }
        }
        let content = format!("{}\n", lines.join("\n"));
        fs::write(Path::new(&path), content).map_err(|error| error.to_string())
    }
}

fn env_value(value: &Value) -> Result<String, String> {
    match value {
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(value.clone()),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| "List settings must contain strings".to_string())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|values| values.join(",")),
        _ => Err("Settings must contain booleans, numbers, or string lists".to_string()),
    }
}

fn validate_admin_settings(values: &serde_json::Map<String, Value>) -> Result<(), String> {
    for key in ADMIN_SETTING_KEYS {
        let Some(value) = values.get(*key) else {
            continue;
        };
        match *key {
            "force_demo" | "invert_sides" | "demo_fallback" => {
                if !value.is_boolean() {
                    return Err(format!("{key} must be boolean"));
                }
            }
            "demo_symbols" => {
                if !value.is_array()
                    || value
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| !item.is_string())
                {
                    return Err("demo_symbols must be a string list".to_string());
                }
            }
            "trading_mode" => {
                if !value
                    .as_str()
                    .map(|mode| matches!(mode, "paper" | "replay" | "simulator"))
                    .unwrap_or(false)
                {
                    return Err("trading_mode must remain paper, replay, or simulator".to_string());
                }
            }
            "strategy_mode" => {
                if !value
                    .as_str()
                    .map(|mode| matches!(mode, "alternative" | "fvg"))
                    .unwrap_or(false)
                {
                    return Err("strategy_mode must be alternative or fvg".to_string());
                }
            }
            _ => {
                if !value.as_f64().map(f64::is_finite).unwrap_or(false) {
                    return Err(format!("{key} must be a finite number"));
                }
            }
        }
    }
    for (target_key, minimum_key) in [
        (
            "alternative_target_r_multiple",
            "alternative_min_net_reward_risk",
        ),
        ("fvg_target_r_multiple", "fvg_min_net_reward_risk"),
    ] {
        if values
            .get(target_key)
            .and_then(Value::as_f64)
            .zip(values.get(minimum_key).and_then(Value::as_f64))
            .is_some_and(|(target, minimum)| target <= minimum || minimum <= 0.0)
        {
            return Err(format!(
                "{target_key} must be greater than {minimum_key} > 0"
            ));
        }
    }
    if values
        .get("fvg_entry_depth")
        .and_then(Value::as_f64)
        .is_some_and(|depth| !(0.0..=1.0).contains(&depth))
    {
        return Err("fvg_entry_depth must be between 0 and 1".to_string());
    }
    Ok(())
}

pub static SETTINGS: LazyLock<Settings> = LazyLock::new(|| {
    load_env_file();
    let settings = Settings::new();
    settings.assert_safe_mode();
    settings
});
