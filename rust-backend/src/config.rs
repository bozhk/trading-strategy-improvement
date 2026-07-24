use std::env;
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

#[derive(Debug, Clone)]
pub struct Settings {
    pub rest_url: String,
    pub ws_url: String,
    pub host: String,
    pub port: u16,
    pub force_demo: bool,
    pub demo_fallback: bool,
    pub trading_mode: String,
    pub scan_interval: u64,
    pub max_symbols: usize,
    pub min_turnover: f64,
    pub min_price: f64,
    pub max_spread_pct: f64,
    pub ws_chunk_size: usize,
    pub wall_multiplier: f64,

    // Cost-aware execution.
    pub taker_fee_pct: f64,
    pub slippage_pct: f64,
    pub cost_safety_multiplier: f64,

    // R-based targets and stops derived from absorbed structure.
    pub min_net_reward_risk: f64,
    pub target_r_multiple: f64,
    pub max_target_pct: f64,
    pub max_holding_seconds: f64,
    pub min_stop_pct: f64,
    pub max_stop_pct: f64,
    pub structure_buffer_pct: f64,
    pub trail_arm_r: f64,
    pub trail_giveback_r: f64,
    pub breakeven_arm_r: f64,

    // Portfolio circuit breakers.
    pub account_equity: f64,
    pub risk_per_trade_pct: f64,
    pub max_position_notional: f64,
    pub max_open_positions: usize,
    pub max_daily_loss_pct: f64,
    pub max_consecutive_losses: usize,
    pub cooldown_seconds: f64,
    pub loss_cooldown_seconds: f64,

    // Strict confirmation sequence and readiness gate.
    pub signal_expiry_seconds: f64,
    pub retest_tolerance_pct: f64,
    pub retest_hold_ticks: u32,
    pub min_confluence_score: i64,
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
            demo_fallback: env_bool("DEMO_FALLBACK", true),
            trading_mode: env_str("TRADING_MODE", "paper").to_lowercase(),
            scan_interval: env_u64("SCAN_INTERVAL", 3600),
            max_symbols: env_u64("MAX_SYMBOLS", 40) as usize,
            min_turnover: 10_000_000.0,
            min_price: 0.005,
            max_spread_pct: 0.10,
            ws_chunk_size: 20,
            wall_multiplier: 12.0,
            taker_fee_pct: env_f64("TAKER_FEE_PCT", 0.00050),
            slippage_pct: env_f64("SLIPPAGE_PCT", 0.00030),
            cost_safety_multiplier: 1.35,
            min_net_reward_risk: env_f64("MIN_NET_REWARD_RISK", 1.35),
            target_r_multiple: env_f64("TARGET_R_MULTIPLE", 1.8),
            // A 15-minute order-flow setup should not manufacture an
            // arbitrarily distant target just to pass the cost gate.
            max_target_pct: env_f64("MAX_TARGET_PCT", 0.0100),
            max_holding_seconds: env_f64("MAX_HOLDING_SECONDS", 900.0),
            min_stop_pct: 0.0020,
            max_stop_pct: 0.0075,
            structure_buffer_pct: 0.0008,
            trail_arm_r: 1.0,
            trail_giveback_r: 0.55,
            breakeven_arm_r: 0.8,
            account_equity: env_f64("ACCOUNT_EQUITY", env_f64("PAPER_EQUITY", 10_000.0)),
            risk_per_trade_pct: env_f64("RISK_PER_TRADE_PCT", 0.0025),
            max_position_notional: 1_000.0,
            max_open_positions: 3,
            max_daily_loss_pct: 0.015,
            max_consecutive_losses: 4,
            cooldown_seconds: 180.0,
            loss_cooldown_seconds: 600.0,
            signal_expiry_seconds: 8.0,
            retest_tolerance_pct: 0.0008,
            retest_hold_ticks: 2,
            min_confluence_score: 75,
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
            self.target_r_multiple > self.min_net_reward_risk && self.min_net_reward_risk > 0.0,
            "TARGET_R_MULTIPLE must be greater than MIN_NET_REWARD_RISK > 0"
        );
        assert!(self.max_target_pct > 0.0, "MAX_TARGET_PCT must be positive");
        assert!(
            self.max_holding_seconds > 0.0,
            "MAX_HOLDING_SECONDS must be positive"
        );
        assert!(
            self.account_equity > 0.0 && self.risk_per_trade_pct > 0.0,
            "ACCOUNT_EQUITY and RISK_PER_TRADE_PCT must be positive"
        );
    }
}

pub static SETTINGS: LazyLock<Settings> = LazyLock::new(|| {
    let settings = Settings::new();
    settings.assert_safe_mode();
    settings
});
