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

fn env_list(name: &str, default: &[&str]) -> Vec<String> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_uppercase())
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| default.iter().map(|value| value.to_string()).collect())
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
    "settings_profile",
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
    "fvg_m15_min_gap_atr",
    "fvg_m15_min_displacement_atr",
    "fvg_m15_min_body_ratio",
    "fvg_m15_max_age_bars",
    "fvg_m1_min_gap_atr",
    "fvg_m1_min_displacement_atr",
    "fvg_m1_min_body_ratio",
    "fvg_m1_confirmation_bars",
    "fvg_s5_min_gap_pct",
    "fvg_s5_min_body_ratio",
    "fvg_s5_inversion_bars",
    "fvg_s5_retest_bars",
    "fvg_s5_entry_depth",
    "min_live_trades",
    "min_live_profit_factor",
    "min_live_expectancy",
    "max_live_drawdown_pct",
];

fn ai_managed_keys() -> &'static [&'static str] {
    &[
        "invert_sides",
        "risk_per_trade_pct",
        "max_open_positions",
        "max_daily_loss_pct",
        "max_consecutive_losses",
        "cooldown_seconds",
        "loss_cooldown_seconds",
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
        "fvg_min_net_reward_risk",
        "fvg_target_r_multiple",
        "fvg_max_target_pct",
        "fvg_min_stop_pct",
        "fvg_max_stop_pct",
        "fvg_stop_buffer_pct",
        "fvg_m15_min_gap_atr",
        "fvg_m15_min_displacement_atr",
        "fvg_m15_min_body_ratio",
        "fvg_m15_max_age_bars",
        "fvg_m1_min_gap_atr",
        "fvg_m1_min_displacement_atr",
        "fvg_m1_min_body_ratio",
        "fvg_m1_confirmation_bars",
        "fvg_s5_min_gap_pct",
        "fvg_s5_min_body_ratio",
        "fvg_s5_inversion_bars",
        "fvg_s5_retest_bars",
        "fvg_s5_entry_depth",
    ]
}

fn is_ai_managed(key: &str) -> bool {
    ai_managed_keys().contains(&key)
}

fn ai_recommended_settings() -> Value {
    json!({
        "invert_sides": false,
        "risk_per_trade_pct": 0.0025,
        "max_open_positions": 2,
        "max_daily_loss_pct": 0.015,
        "max_consecutive_losses": 4,
        "cooldown_seconds": 180.0,
        "loss_cooldown_seconds": 900.0,
        "cost_safety_multiplier": 1.35,
        "max_holding_seconds": 900.0,
        "trail_arm_r": 1.0,
        "trail_giveback_r": 0.55,
        "breakeven_arm_r": 0.8,
        "reversal_confirm_ticks": 3,
        "reversal_confirm_seconds": 3.0,
        "reversal_min_hold_seconds": 30.0,
        "alternative_wall_multiplier": 15.0,
        "alternative_signal_expiry_seconds": 8.0,
        "alternative_retest_tolerance_pct": 0.0008,
        "alternative_retest_hold_ticks": 3,
        "alternative_min_confluence_score": 85,
        "alternative_min_net_reward_risk": 1.35,
        "alternative_target_r_multiple": 1.8,
        "alternative_max_target_pct": 0.0100,
        "alternative_min_stop_pct": 0.0020,
        "alternative_max_stop_pct": 0.0075,
        "alternative_structure_buffer_pct": 0.0008,
        "fvg_min_net_reward_risk": 1.50,
        "fvg_target_r_multiple": 2.0,
        "fvg_max_target_pct": 0.0120,
        "fvg_min_stop_pct": 0.0015,
        "fvg_max_stop_pct": 0.0060,
        "fvg_stop_buffer_pct": 0.0005,
        "fvg_m15_min_gap_atr": 0.10,
        "fvg_m15_min_displacement_atr": 1.0,
        "fvg_m15_min_body_ratio": 0.60,
        "fvg_m15_max_age_bars": 16,
        "fvg_m1_min_gap_atr": 0.05,
        "fvg_m1_min_displacement_atr": 0.8,
        "fvg_m1_min_body_ratio": 0.60,
        "fvg_m1_confirmation_bars": 15,
        "fvg_s5_min_gap_pct": 0.00015,
        "fvg_s5_min_body_ratio": 0.55,
        "fvg_s5_inversion_bars": 24,
        "fvg_s5_retest_bars": 12,
        "fvg_s5_entry_depth": 0.50
    })
}

fn saved_custom_settings() -> Value {
    let mut values = ai_recommended_settings();
    let custom_defaults = json!({
        "fvg_min_net_reward_risk": 1.35,
        "fvg_target_r_multiple": 1.8,
        "fvg_max_target_pct": 0.0100,
        "fvg_min_stop_pct": 0.0020,
        "fvg_max_stop_pct": 0.0075,
        "fvg_stop_buffer_pct": 0.0008
    });
    for (key, value) in custom_defaults.as_object().unwrap() {
        values[key] = value.clone();
    }
    for key in ai_managed_keys() {
        let Ok(raw) = env::var(key.to_uppercase()) else {
            continue;
        };
        if values[*key].is_boolean() {
            values[*key] = json!(matches!(
                raw.to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ));
        } else if let Ok(number) = raw.parse::<f64>() {
            if number.is_finite() {
                values[*key] = json!(number);
            }
        }
    }
    values
}

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
    pub settings_profile: String,
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

    // Legacy single-timeframe FVG settings retained for persisted configuration.
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

    // M15 context -> M1 confirmation -> S5 inverse-FVG retest.
    pub fvg_m15_min_gap_atr: f64,
    pub fvg_m15_min_displacement_atr: f64,
    pub fvg_m15_min_body_ratio: f64,
    pub fvg_m15_max_age_bars: u32,
    pub fvg_m1_min_gap_atr: f64,
    pub fvg_m1_min_displacement_atr: f64,
    pub fvg_m1_min_body_ratio: f64,
    pub fvg_m1_confirmation_bars: u32,
    pub fvg_s5_min_gap_pct: f64,
    pub fvg_s5_min_body_ratio: f64,
    pub fvg_s5_inversion_bars: u32,
    pub fvg_s5_retest_bars: u32,
    pub fvg_s5_entry_depth: f64,

    // Readiness gate.
    pub min_live_trades: usize,
    pub min_live_profit_factor: f64,
    pub min_live_expectancy: f64,
    pub max_live_drawdown_pct: f64,
    pub demo_symbols: Vec<String>,
}

impl Settings {
    fn new() -> Self {
        let mut settings = Self {
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
            settings_profile: env_str("SETTINGS_PROFILE", "custom").to_lowercase(),
            scan_interval: env_u64("SCAN_INTERVAL", 3600),
            max_symbols: env_u64("MAX_SYMBOLS", 40) as usize,
            min_turnover: env_f64("MIN_TURNOVER", 10_000_000.0),
            min_price: env_f64("MIN_PRICE", 0.005),
            max_spread_pct: env_f64("MAX_SPREAD_PCT", 0.10),
            ws_chunk_size: env_u64("WS_CHUNK_SIZE", 20) as usize,
            taker_fee_pct: env_f64("TAKER_FEE_PCT", 0.00050),
            slippage_pct: env_f64("SLIPPAGE_PCT", 0.00030),
            cost_safety_multiplier: env_f64("COST_SAFETY_MULTIPLIER", 1.35),
            max_holding_seconds: env_f64("MAX_HOLDING_SECONDS", 900.0),
            trail_arm_r: env_f64("TRAIL_ARM_R", 1.0),
            trail_giveback_r: env_f64("TRAIL_GIVEBACK_R", 0.55),
            breakeven_arm_r: env_f64("BREAKEVEN_ARM_R", 0.8),
            reversal_confirm_ticks: env_u64("REVERSAL_CONFIRM_TICKS", 3) as u32,
            reversal_confirm_seconds: env_f64("REVERSAL_CONFIRM_SECONDS", 3.0),
            reversal_min_hold_seconds: env_f64("REVERSAL_MIN_HOLD_SECONDS", 30.0),
            account_equity: env_f64("ACCOUNT_EQUITY", env_f64("PAPER_EQUITY", 10_000.0)),
            risk_per_trade_pct: env_f64("RISK_PER_TRADE_PCT", 0.0025),
            max_position_notional: env_f64("MAX_POSITION_NOTIONAL", 1_000.0),
            max_open_positions: env_u64("MAX_OPEN_POSITIONS", 2) as usize,
            max_daily_loss_pct: env_f64("MAX_DAILY_LOSS_PCT", 0.015),
            max_consecutive_losses: env_u64("MAX_CONSECUTIVE_LOSSES", 4) as usize,
            cooldown_seconds: env_f64("COOLDOWN_SECONDS", 180.0),
            loss_cooldown_seconds: env_f64("LOSS_COOLDOWN_SECONDS", 900.0),
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
            fvg_candle_seconds: env_f64("FVG_CANDLE_SECONDS", 300.0),
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
            fvg_m15_min_gap_atr: env_f64("FVG_M15_MIN_GAP_ATR", 0.10),
            fvg_m15_min_displacement_atr: env_f64("FVG_M15_MIN_DISPLACEMENT_ATR", 1.0),
            fvg_m15_min_body_ratio: env_f64("FVG_M15_MIN_BODY_RATIO", 0.60),
            fvg_m15_max_age_bars: env_u64("FVG_M15_MAX_AGE_BARS", 16) as u32,
            fvg_m1_min_gap_atr: env_f64("FVG_M1_MIN_GAP_ATR", 0.05),
            fvg_m1_min_displacement_atr: env_f64("FVG_M1_MIN_DISPLACEMENT_ATR", 0.8),
            fvg_m1_min_body_ratio: env_f64("FVG_M1_MIN_BODY_RATIO", 0.60),
            fvg_m1_confirmation_bars: env_u64("FVG_M1_CONFIRMATION_BARS", 15) as u32,
            fvg_s5_min_gap_pct: env_f64("FVG_S5_MIN_GAP_PCT", 0.00015),
            fvg_s5_min_body_ratio: env_f64("FVG_S5_MIN_BODY_RATIO", 0.55),
            fvg_s5_inversion_bars: env_u64("FVG_S5_INVERSION_BARS", 24) as u32,
            fvg_s5_retest_bars: env_u64("FVG_S5_RETEST_BARS", 12) as u32,
            fvg_s5_entry_depth: env_f64("FVG_S5_ENTRY_DEPTH", 0.50),
            min_live_trades: env_u64("MIN_LIVE_TRADES", 200) as usize,
            min_live_profit_factor: env_f64("MIN_LIVE_PROFIT_FACTOR", 1.20),
            min_live_expectancy: env_f64("MIN_LIVE_EXPECTANCY", 0.0),
            max_live_drawdown_pct: env_f64("MAX_LIVE_DRAWDOWN_PCT", 0.10),
            demo_symbols: env_list(
                "DEMO_SYMBOLS",
                &[
                    "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "DOGEUSDT", "LINKUSDT", "AVAXUSDT",
                    "SUIUSDT",
                ],
            ),
        };
        if settings.settings_profile == "ai_recommended" {
            settings.apply_ai_recommended();
        }
        settings
    }

    fn apply_ai_recommended(&mut self) {
        let values = ai_recommended_settings();
        let number = |key: &str| values[key].as_f64().expect("AI setting must be numeric");
        self.invert_sides = false;
        self.risk_per_trade_pct = number("risk_per_trade_pct");
        self.max_open_positions = number("max_open_positions") as usize;
        self.max_daily_loss_pct = number("max_daily_loss_pct");
        self.max_consecutive_losses = number("max_consecutive_losses") as usize;
        self.cooldown_seconds = number("cooldown_seconds");
        self.loss_cooldown_seconds = number("loss_cooldown_seconds");
        self.cost_safety_multiplier = number("cost_safety_multiplier");
        self.max_holding_seconds = number("max_holding_seconds");
        self.trail_arm_r = number("trail_arm_r");
        self.trail_giveback_r = number("trail_giveback_r");
        self.breakeven_arm_r = number("breakeven_arm_r");
        self.reversal_confirm_ticks = number("reversal_confirm_ticks") as u32;
        self.reversal_confirm_seconds = number("reversal_confirm_seconds");
        self.reversal_min_hold_seconds = number("reversal_min_hold_seconds");
        self.alternative_wall_multiplier = number("alternative_wall_multiplier");
        self.alternative_signal_expiry_seconds = number("alternative_signal_expiry_seconds");
        self.alternative_retest_tolerance_pct = number("alternative_retest_tolerance_pct");
        self.alternative_retest_hold_ticks = number("alternative_retest_hold_ticks") as u32;
        self.alternative_min_confluence_score = number("alternative_min_confluence_score") as i64;
        self.alternative_min_net_reward_risk = number("alternative_min_net_reward_risk");
        self.alternative_target_r_multiple = number("alternative_target_r_multiple");
        self.alternative_max_target_pct = number("alternative_max_target_pct");
        self.alternative_min_stop_pct = number("alternative_min_stop_pct");
        self.alternative_max_stop_pct = number("alternative_max_stop_pct");
        self.alternative_structure_buffer_pct = number("alternative_structure_buffer_pct");
        self.fvg_min_net_reward_risk = number("fvg_min_net_reward_risk");
        self.fvg_target_r_multiple = number("fvg_target_r_multiple");
        self.fvg_max_target_pct = number("fvg_max_target_pct");
        self.fvg_min_stop_pct = number("fvg_min_stop_pct");
        self.fvg_max_stop_pct = number("fvg_max_stop_pct");
        self.fvg_stop_buffer_pct = number("fvg_stop_buffer_pct");
        self.fvg_m15_min_gap_atr = number("fvg_m15_min_gap_atr");
        self.fvg_m15_min_displacement_atr = number("fvg_m15_min_displacement_atr");
        self.fvg_m15_min_body_ratio = number("fvg_m15_min_body_ratio");
        self.fvg_m15_max_age_bars = number("fvg_m15_max_age_bars") as u32;
        self.fvg_m1_min_gap_atr = number("fvg_m1_min_gap_atr");
        self.fvg_m1_min_displacement_atr = number("fvg_m1_min_displacement_atr");
        self.fvg_m1_min_body_ratio = number("fvg_m1_min_body_ratio");
        self.fvg_m1_confirmation_bars = number("fvg_m1_confirmation_bars") as u32;
        self.fvg_s5_min_gap_pct = number("fvg_s5_min_gap_pct");
        self.fvg_s5_min_body_ratio = number("fvg_s5_min_body_ratio");
        self.fvg_s5_inversion_bars = number("fvg_s5_inversion_bars") as u32;
        self.fvg_s5_retest_bars = number("fvg_s5_retest_bars") as u32;
        self.fvg_s5_entry_depth = number("fvg_s5_entry_depth");
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
            matches!(self.settings_profile.as_str(), "custom" | "ai_recommended"),
            "SETTINGS_PROFILE must be custom or ai_recommended"
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
            self.alternative_min_stop_pct > 0.0
                && self.alternative_min_stop_pct <= self.alternative_max_stop_pct
                && self.fvg_min_stop_pct > 0.0
                && self.fvg_min_stop_pct <= self.fvg_max_stop_pct,
            "Strategy stop ranges are invalid"
        );
        assert!(self.ws_chunk_size > 0, "WS_CHUNK_SIZE must be positive");
        assert!(
            self.fvg_candle_seconds > 0.0
                && self.fvg_min_gap_pct > 0.0
                && self.fvg_signal_expiry_seconds > 0.0
                && (0.0..=1.0).contains(&self.fvg_entry_depth),
            "FVG settings are invalid"
        );
        assert!(
            self.fvg_m15_min_gap_atr > 0.0
                && self.fvg_m15_min_displacement_atr > 0.0
                && (0.0..=1.0).contains(&self.fvg_m15_min_body_ratio)
                && self.fvg_m15_max_age_bars > 0
                && self.fvg_m1_min_gap_atr > 0.0
                && self.fvg_m1_min_displacement_atr > 0.0
                && (0.0..=1.0).contains(&self.fvg_m1_min_body_ratio)
                && self.fvg_m1_confirmation_bars > 0
                && self.fvg_s5_min_gap_pct > 0.0
                && (0.0..=1.0).contains(&self.fvg_s5_min_body_ratio)
                && self.fvg_s5_inversion_bars > 0
                && self.fvg_s5_retest_bars > 0
                && (0.0..=1.0).contains(&self.fvg_s5_entry_depth),
            "MTF FVG settings are invalid"
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
            "settings_profile": self.settings_profile,
            "_ai_recommended": ai_recommended_settings(),
            "_ai_managed_keys": ai_managed_keys(),
            "_custom_settings": saved_custom_settings(),
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
            "fvg_m15_min_gap_atr": self.fvg_m15_min_gap_atr,
            "fvg_m15_min_displacement_atr": self.fvg_m15_min_displacement_atr,
            "fvg_m15_min_body_ratio": self.fvg_m15_min_body_ratio,
            "fvg_m15_max_age_bars": self.fvg_m15_max_age_bars,
            "fvg_m1_min_gap_atr": self.fvg_m1_min_gap_atr,
            "fvg_m1_min_displacement_atr": self.fvg_m1_min_displacement_atr,
            "fvg_m1_min_body_ratio": self.fvg_m1_min_body_ratio,
            "fvg_m1_confirmation_bars": self.fvg_m1_confirmation_bars,
            "fvg_s5_min_gap_pct": self.fvg_s5_min_gap_pct,
            "fvg_s5_min_body_ratio": self.fvg_s5_min_body_ratio,
            "fvg_s5_inversion_bars": self.fvg_s5_inversion_bars,
            "fvg_s5_retest_bars": self.fvg_s5_retest_bars,
            "fvg_s5_entry_depth": self.fvg_s5_entry_depth,
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
        let requested_profile = object
            .get("settings_profile")
            .and_then(Value::as_str)
            .unwrap_or(&self.settings_profile);
        let mut validated = object.clone();
        if requested_profile == "ai_recommended" {
            validated.retain(|key, _| !is_ai_managed(key));
        }
        validate_admin_settings(&validated)?;

        let path = env_file_path();
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
        for key in ADMIN_SETTING_KEYS {
            let Some(value) = object.get(*key) else {
                continue;
            };
            if requested_profile == "ai_recommended" && is_ai_managed(key) {
                continue;
            }
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
            "settings_profile" => {
                if !value
                    .as_str()
                    .is_some_and(|profile| matches!(profile, "custom" | "ai_recommended"))
                {
                    return Err("settings_profile must be custom or ai_recommended".to_string());
                }
            }
            "ws_chunk_size"
            | "fvg_m15_max_age_bars"
            | "fvg_m1_confirmation_bars"
            | "fvg_s5_inversion_bars"
            | "fvg_s5_retest_bars" => {
                if !value.as_u64().is_some_and(|value| value > 0) {
                    return Err(format!("{key} must be a positive integer"));
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
    for key in [
        "fvg_m15_min_body_ratio",
        "fvg_m1_min_body_ratio",
        "fvg_s5_min_body_ratio",
        "fvg_s5_entry_depth",
    ] {
        if values
            .get(key)
            .and_then(Value::as_f64)
            .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            return Err(format!("{key} must be between 0 and 1"));
        }
    }
    for key in [
        "fvg_m15_min_gap_atr",
        "fvg_m15_min_displacement_atr",
        "fvg_m1_min_gap_atr",
        "fvg_m1_min_displacement_atr",
        "fvg_s5_min_gap_pct",
    ] {
        if values
            .get(key)
            .and_then(Value::as_f64)
            .is_some_and(|value| value <= 0.0)
        {
            return Err(format!("{key} must be positive"));
        }
    }
    for (minimum_key, maximum_key) in [
        ("alternative_min_stop_pct", "alternative_max_stop_pct"),
        ("fvg_min_stop_pct", "fvg_max_stop_pct"),
    ] {
        if values
            .get(minimum_key)
            .and_then(Value::as_f64)
            .zip(values.get(maximum_key).and_then(Value::as_f64))
            .is_some_and(|(minimum, maximum)| minimum <= 0.0 || minimum > maximum)
        {
            return Err(format!(
                "{minimum_key} must be positive and no greater than {maximum_key}"
            ));
        }
    }
    Ok(())
}

pub static SETTINGS: LazyLock<Settings> = LazyLock::new(|| {
    load_env_file();
    let settings = Settings::new();
    settings.assert_safe_mode();
    settings
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_preset_defines_every_managed_setting() {
        let preset = ai_recommended_settings();
        for key in ai_managed_keys() {
            assert_ne!(preset[*key], Value::Null, "missing AI preset for {key}");
        }
    }

    #[test]
    fn ai_preset_keeps_risk_geometry_valid() {
        let preset = ai_recommended_settings();
        assert!(
            preset["fvg_target_r_multiple"].as_f64().unwrap()
                > preset["fvg_min_net_reward_risk"].as_f64().unwrap()
        );
        assert!(
            preset["fvg_min_stop_pct"].as_f64().unwrap()
                <= preset["fvg_max_stop_pct"].as_f64().unwrap()
        );
        assert!((0.0..=1.0).contains(&preset["fvg_s5_entry_depth"].as_f64().unwrap()));
    }

    #[test]
    fn unknown_settings_profile_is_rejected() {
        let mut values = serde_json::Map::new();
        values.insert("settings_profile".to_string(), json!("automatic"));
        assert!(validate_admin_settings(&values).is_err());
    }
}
