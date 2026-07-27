use crate::config::Settings;
use crate::models::{Side, TradeTick};
use serde_json::{json, Value};
use std::collections::VecDeque;

const ATR_PERIOD: usize = 14;
const S5_RETENTION: usize = 720;
const M1_RETENTION: usize = 240;
const M15_RETENTION: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Timeframe {
    S5,
    M1,
    M15,
}

impl Timeframe {
    pub const fn seconds(self) -> i64 {
        match self {
            Self::S5 => 5,
            Self::M1 => 60,
            Self::M15 => 900,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::S5 => "S5",
            Self::M1 => "M1",
            Self::M15 => "M15",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Candle {
    pub timeframe: Timeframe,
    pub started_at: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub trades: u64,
}

impl Candle {
    pub fn is_closed(self, now: f64) -> bool {
        self.started_at + self.timeframe.seconds() <= now.floor() as i64
    }
}

#[derive(Debug, Clone, Default)]
pub struct MtfBars {
    pub s5: VecDeque<Candle>,
    pub m1: VecDeque<Candle>,
    pub m15: VecDeque<Candle>,
    pub valid_from: Option<i64>,
}

impl MtfBars {
    pub fn update(&mut self, tick: &TradeTick) {
        if !tick.timestamp.is_finite()
            || !tick.price.is_finite()
            || tick.price <= 0.0
            || !tick.size.is_finite()
            || tick.size < 0.0
        {
            return;
        }
        update_series(&mut self.s5, Timeframe::S5, S5_RETENTION, tick);
        update_series(&mut self.m1, Timeframe::M1, M1_RETENTION, tick);
        update_series(&mut self.m15, Timeframe::M15, M15_RETENTION, tick);
    }

    pub fn seed(&mut self, timeframe: Timeframe, candles: impl IntoIterator<Item = Candle>) {
        let (series, retention) = match timeframe {
            Timeframe::S5 => (&mut self.s5, S5_RETENTION),
            Timeframe::M1 => (&mut self.m1, M1_RETENTION),
            Timeframe::M15 => (&mut self.m15, M15_RETENTION),
        };
        for candle in candles {
            if candle.timeframe != timeframe
                || !candle.open.is_finite()
                || !candle.high.is_finite()
                || !candle.low.is_finite()
                || !candle.close.is_finite()
                || candle.low <= 0.0
                || candle.low > candle.high
            {
                continue;
            }
            if let Some(existing) = series
                .iter_mut()
                .find(|existing| existing.started_at == candle.started_at)
            {
                *existing = candle;
            } else {
                series.push_back(candle);
            }
        }
        series
            .make_contiguous()
            .sort_by_key(|candle| candle.started_at);
        while series.len() > retention {
            series.pop_front();
        }
    }

    fn series(&self, timeframe: Timeframe) -> &VecDeque<Candle> {
        match timeframe {
            Timeframe::S5 => &self.s5,
            Timeframe::M1 => &self.m1,
            Timeframe::M15 => &self.m15,
        }
    }

    pub fn handle_disconnect(&mut self, now: f64) {
        self.s5.clear();
        self.m1.retain(|candle| candle.is_closed(now));
        self.m15.retain(|candle| candle.is_closed(now));
        self.valid_from = Some(now.floor() as i64);
    }

    pub fn mark_history_valid(&mut self) {
        self.valid_from = None;
    }
}

fn update_series(
    series: &mut VecDeque<Candle>,
    timeframe: Timeframe,
    retention: usize,
    tick: &TradeTick,
) {
    let duration = timeframe.seconds();
    let started_at = (tick.timestamp.floor() as i64).div_euclid(duration) * duration;
    if let Some(candle) = series
        .back_mut()
        .filter(|candle| candle.started_at == started_at)
    {
        candle.high = candle.high.max(tick.price);
        candle.low = candle.low.min(tick.price);
        candle.close = tick.price;
        candle.volume += tick.size;
        candle.trades += 1;
        return;
    }
    if series
        .back()
        .is_some_and(|candle| started_at <= candle.started_at)
    {
        return;
    }
    if series.len() >= retention {
        series.pop_front();
    }
    series.push_back(Candle {
        timeframe,
        started_at,
        open: tick.price,
        high: tick.price,
        low: tick.price,
        close: tick.price,
        volume: tick.size,
        trades: 1,
    });
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ZoneId {
    pub timeframe: Timeframe,
    pub first_started_at: i64,
    pub third_started_at: i64,
    pub side: Side,
}

#[derive(Debug, Clone)]
pub struct FvgZone {
    pub id: ZoneId,
    pub side: Side,
    pub lower: f64,
    pub upper: f64,
    pub created_at: i64,
    pub middle_range_atr: f64,
    pub middle_body_ratio: f64,
    pub gap_atr: f64,
}

#[derive(Debug, Clone)]
pub struct MtfSetup {
    pub side: Side,
    pub context: FvgZone,
    pub confirmation: FvgZone,
    pub trigger: FvgZone,
    pub context_touched_at: i64,
    pub inverted_at: i64,
    pub retested_at: i64,
    pub entry_deadline: i64,
    pub entry_level: f64,
}

#[derive(Debug, Clone)]
pub enum MtfPhase {
    WaitingContext,
    WaitingContextTouch {
        context: FvgZone,
        approached: bool,
    },
    WaitingM1 {
        context: FvgZone,
        touched_at: i64,
        first_eligible_m1: i64,
        deadline: i64,
    },
    WaitingS5Candidate {
        context: FvgZone,
        confirmation: FvgZone,
        touched_at: i64,
        deadline: i64,
    },
    WaitingS5Inversion {
        context: FvgZone,
        confirmation: FvgZone,
        trigger: FvgZone,
        touched_at: i64,
        deadline: i64,
    },
    WaitingS5Retest {
        context: FvgZone,
        confirmation: FvgZone,
        trigger: FvgZone,
        touched_at: i64,
        inverted_at: i64,
        deadline: i64,
    },
    Ready(MtfSetup),
}

#[derive(Debug, Clone)]
pub struct MtfFvgTracker {
    pub phase: MtfPhase,
    consumed_contexts: VecDeque<ZoneId>,
    pub last_reason: String,
}

impl Default for MtfFvgTracker {
    fn default() -> Self {
        Self {
            phase: MtfPhase::WaitingContext,
            consumed_contexts: VecDeque::new(),
            last_reason: "waiting for M15 context".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Quality {
    min_gap_atr: f64,
    min_displacement_atr: f64,
    min_body_ratio: f64,
}

#[derive(Debug, Clone)]
pub struct MtfConfig {
    pub m15_min_gap_atr: f64,
    pub m15_min_displacement_atr: f64,
    pub m15_min_body_ratio: f64,
    pub m15_max_age_bars: i64,
    pub m1_min_gap_atr: f64,
    pub m1_min_displacement_atr: f64,
    pub m1_min_body_ratio: f64,
    pub m1_confirmation_bars: i64,
    pub s5_min_gap_pct: f64,
    pub s5_min_body_ratio: f64,
    pub s5_inversion_bars: i64,
    pub s5_retest_bars: i64,
    pub s5_entry_depth: f64,
    pub invalidation_buffer_pct: f64,
}

impl From<&Settings> for MtfConfig {
    fn from(settings: &Settings) -> Self {
        Self {
            m15_min_gap_atr: settings.fvg_m15_min_gap_atr,
            m15_min_displacement_atr: settings.fvg_m15_min_displacement_atr,
            m15_min_body_ratio: settings.fvg_m15_min_body_ratio,
            m15_max_age_bars: settings.fvg_m15_max_age_bars as i64,
            m1_min_gap_atr: settings.fvg_m1_min_gap_atr,
            m1_min_displacement_atr: settings.fvg_m1_min_displacement_atr,
            m1_min_body_ratio: settings.fvg_m1_min_body_ratio,
            m1_confirmation_bars: settings.fvg_m1_confirmation_bars as i64,
            s5_min_gap_pct: settings.fvg_s5_min_gap_pct,
            s5_min_body_ratio: settings.fvg_s5_min_body_ratio,
            s5_inversion_bars: settings.fvg_s5_inversion_bars as i64,
            s5_retest_bars: settings.fvg_s5_retest_bars as i64,
            s5_entry_depth: settings.fvg_s5_entry_depth,
            invalidation_buffer_pct: settings.fvg_stop_buffer_pct,
        }
    }
}

fn atr_at(candles: &[Candle], end: usize) -> Option<f64> {
    if end < ATR_PERIOD || end >= candles.len() {
        return None;
    }
    let start = end + 1 - ATR_PERIOD;
    let mut total = 0.0;
    for index in start..=end {
        let candle = candles[index];
        let previous_close = if index == 0 {
            candle.open
        } else {
            candles[index - 1].close
        };
        total += (candle.high - candle.low)
            .max((candle.high - previous_close).abs())
            .max((candle.low - previous_close).abs());
    }
    let atr = total / ATR_PERIOD as f64;
    (atr.is_finite() && atr > 0.0).then_some(atr)
}

fn detect_at(candles: &[Candle], third: usize, quality: Quality) -> Option<FvgZone> {
    if third < 2 {
        return None;
    }
    let first = candles[third - 2];
    let middle = candles[third - 1];
    let last = candles[third];
    let timeframe = first.timeframe;
    let duration = timeframe.seconds();
    if middle.timeframe != timeframe
        || last.timeframe != timeframe
        || middle.started_at != first.started_at + duration
        || last.started_at != middle.started_at + duration
    {
        return None;
    }
    let (side, lower, upper) = if last.low > first.high {
        (Side::Long, first.high, last.low)
    } else if last.high < first.low {
        (Side::Short, last.high, first.low)
    } else {
        return None;
    };
    let directional_middle = match side {
        Side::Long => middle.close > middle.open,
        Side::Short => middle.close < middle.open,
    };
    let range = middle.high - middle.low;
    if !directional_middle || range <= 0.0 {
        return None;
    }
    let body_ratio = (middle.close - middle.open).abs() / range;
    let atr = atr_at(candles, third)?;
    let gap_atr = (upper - lower) / atr;
    let displacement_atr = range / atr;
    if gap_atr < quality.min_gap_atr
        || displacement_atr < quality.min_displacement_atr
        || body_ratio < quality.min_body_ratio
    {
        return None;
    }
    Some(FvgZone {
        id: ZoneId {
            timeframe,
            first_started_at: first.started_at,
            third_started_at: last.started_at,
            side,
        },
        side,
        lower,
        upper,
        created_at: last.started_at + duration,
        middle_range_atr: displacement_atr,
        middle_body_ratio: body_ratio,
        gap_atr,
    })
}

fn detected_zones(
    bars: &MtfBars,
    timeframe: Timeframe,
    now: f64,
    quality: Quality,
) -> Vec<FvgZone> {
    let candles: Vec<Candle> = bars
        .series(timeframe)
        .iter()
        .copied()
        .filter(|candle| candle.is_closed(now))
        .collect();
    (2..candles.len())
        .filter_map(|third| detect_at(&candles, third, quality))
        .collect()
}

fn detected_s5_zones(bars: &MtfBars, now: f64, config: &MtfConfig) -> Vec<FvgZone> {
    let candles: Vec<Candle> = bars
        .s5
        .iter()
        .copied()
        .filter(|candle| candle.is_closed(now))
        .collect();
    let mut result = Vec::new();
    for third in 2..candles.len() {
        let first = candles[third - 2];
        let middle = candles[third - 1];
        let last = candles[third];
        if middle.started_at != first.started_at + 5 || last.started_at != middle.started_at + 5 {
            continue;
        }
        let (side, lower, upper) = if last.low > first.high {
            (Side::Long, first.high, last.low)
        } else if last.high < first.low {
            (Side::Short, last.high, first.low)
        } else {
            continue;
        };
        let range = middle.high - middle.low;
        let directional = (side == Side::Long && middle.close > middle.open)
            || (side == Side::Short && middle.close < middle.open);
        let midpoint = (lower + upper) / 2.0;
        if !directional
            || range <= 0.0
            || (middle.close - middle.open).abs() / range < config.s5_min_body_ratio
            || (upper - lower) / midpoint < config.s5_min_gap_pct
        {
            continue;
        }
        result.push(FvgZone {
            id: ZoneId {
                timeframe: Timeframe::S5,
                first_started_at: first.started_at,
                third_started_at: last.started_at,
                side,
            },
            side,
            lower,
            upper,
            created_at: last.started_at + 5,
            middle_range_atr: 0.0,
            middle_body_ratio: (middle.close - middle.open).abs() / range,
            gap_atr: 0.0,
        });
    }
    result
}

fn next_boundary(timestamp: i64, timeframe: Timeframe) -> i64 {
    let duration = timeframe.seconds();
    timestamp.div_euclid(duration) * duration + duration
}

fn context_invalidated(context: &FvgZone, bars: &MtfBars, mark: f64, buffer_pct: f64) -> bool {
    let breached = match context.side {
        Side::Long => mark < context.lower * (1.0 - buffer_pct),
        Side::Short => mark > context.upper * (1.0 + buffer_pct),
    };
    breached
        || bars
            .s5
            .iter()
            .filter(|candle| candle.started_at >= context.created_at)
            .any(|candle| match context.side {
                Side::Long => candle.low < context.lower * (1.0 - buffer_pct),
                Side::Short => candle.high > context.upper * (1.0 + buffer_pct),
            })
}

fn zone_intersects(candle: Candle, zone: &FvgZone) -> bool {
    candle.low <= zone.upper && candle.high >= zone.lower
}

impl MtfFvgTracker {
    fn consume(&mut self, context: &FvgZone, reason: &str) {
        if self.consumed_contexts.len() >= 64 {
            self.consumed_contexts.pop_front();
        }
        if !self.consumed_contexts.contains(&context.id) {
            self.consumed_contexts.push_back(context.id.clone());
        }
        self.phase = MtfPhase::WaitingContext;
        self.last_reason = reason.to_string();
    }

    pub fn mark_entry_consumed(&mut self) {
        if let MtfPhase::Ready(setup) = self.phase.clone() {
            self.consume(&setup.context, "setup entered");
        }
    }

    pub fn reject_ready(&mut self, reason: &str) {
        if let MtfPhase::Ready(setup) = self.phase.clone() {
            self.consume(&setup.context, reason);
        }
    }

    pub fn ready(&self) -> Option<&MtfSetup> {
        match &self.phase {
            MtfPhase::Ready(setup) => Some(setup),
            _ => None,
        }
    }

    pub fn phase_name(&self) -> &'static str {
        match self.phase {
            MtfPhase::WaitingContext => "WAIT_M15_CONTEXT",
            MtfPhase::WaitingContextTouch { .. } => "WAIT_M15_TOUCH",
            MtfPhase::WaitingM1 { .. } => "WAIT_M1_CONFIRMATION",
            MtfPhase::WaitingS5Candidate { .. } => "WAIT_S5_FVG",
            MtfPhase::WaitingS5Inversion { .. } => "WAIT_S5_INVERSION",
            MtfPhase::WaitingS5Retest { .. } => "WAIT_S5_RETEST",
            MtfPhase::Ready(_) => "ENTRY_READY",
        }
    }

    pub fn snapshot(&self) -> Value {
        let zone_json = |zone: &FvgZone| {
            json!({
                "timeframe": zone.id.timeframe.as_str(), "side": zone.side.as_str(),
                "lower": zone.lower, "upper": zone.upper, "created_at": zone.created_at,
                "gap_atr": zone.gap_atr, "displacement_atr": zone.middle_range_atr,
                "body_ratio": zone.middle_body_ratio,
            })
        };
        let mut value = json!({"phase": self.phase_name(), "reason": self.last_reason});
        let object = value.as_object_mut().unwrap();
        match &self.phase {
            MtfPhase::WaitingContext => {}
            MtfPhase::WaitingContextTouch { context, .. } => {
                object.insert("context".into(), zone_json(context));
            }
            MtfPhase::WaitingM1 {
                context,
                touched_at,
                deadline,
                ..
            } => {
                object.insert("context".into(), zone_json(context));
                object.insert("context_touched_at".into(), json!(touched_at));
                object.insert("deadline".into(), json!(deadline));
            }
            MtfPhase::WaitingS5Candidate {
                context,
                confirmation,
                deadline,
                ..
            }
            | MtfPhase::WaitingS5Inversion {
                context,
                confirmation,
                deadline,
                ..
            } => {
                object.insert("context".into(), zone_json(context));
                object.insert("confirmation".into(), zone_json(confirmation));
                object.insert("deadline".into(), json!(deadline));
            }
            MtfPhase::WaitingS5Retest {
                context,
                confirmation,
                trigger,
                inverted_at,
                deadline,
                ..
            } => {
                object.insert("context".into(), zone_json(context));
                object.insert("confirmation".into(), zone_json(confirmation));
                object.insert("trigger".into(), zone_json(trigger));
                object.insert("inverted_at".into(), json!(inverted_at));
                object.insert("deadline".into(), json!(deadline));
            }
            MtfPhase::Ready(setup) => {
                object.insert("context".into(), zone_json(&setup.context));
                object.insert("confirmation".into(), zone_json(&setup.confirmation));
                object.insert("trigger".into(), zone_json(&setup.trigger));
                object.insert("entry_level".into(), json!(setup.entry_level));
            }
        }
        value
    }

    pub fn advance(&mut self, bars: &MtfBars, now: f64, mark: f64, config: &MtfConfig) {
        let timestamp = now.floor() as i64;
        let phase = self.phase.clone();
        match phase {
            MtfPhase::WaitingContext => {
                let quality = Quality {
                    min_gap_atr: config.m15_min_gap_atr,
                    min_displacement_atr: config.m15_min_displacement_atr,
                    min_body_ratio: config.m15_min_body_ratio,
                };
                let m15_bars: Vec<Candle> = bars.m15.iter().copied().collect();
                let candidate = detected_zones(bars, Timeframe::M15, now, quality)
                    .into_iter()
                    .rev()
                    .find(|zone| {
                        timestamp - zone.created_at
                            <= config.m15_max_age_bars * Timeframe::M15.seconds()
                            && bars
                                .valid_from
                                .map(|valid_from| zone.created_at >= valid_from)
                                .unwrap_or(true)
                            && !self.consumed_contexts.contains(&zone.id)
                            && !m15_bars
                                .iter()
                                .copied()
                                .filter(|bar| bar.started_at >= zone.created_at)
                                .any(|bar| zone_intersects(bar, zone))
                    });
                if let Some(context) = candidate {
                    let approached = match context.side {
                        Side::Long => mark > context.upper,
                        Side::Short => mark < context.lower,
                    };
                    self.last_reason = format!("{} context armed", context.side.as_str());
                    self.phase = MtfPhase::WaitingContextTouch {
                        context,
                        approached,
                    };
                }
            }
            MtfPhase::WaitingContextTouch {
                context,
                mut approached,
            } => {
                if timestamp - context.created_at
                    > config.m15_max_age_bars * Timeframe::M15.seconds()
                {
                    self.consume(&context, "M15 context expired");
                    return;
                }
                if context_invalidated(&context, bars, mark, config.invalidation_buffer_pct) {
                    self.consume(&context, "M15 context invalidated");
                    return;
                }
                approached |= match context.side {
                    Side::Long => mark > context.upper,
                    Side::Short => mark < context.lower,
                };
                approached |= bars
                    .s5
                    .iter()
                    .filter(|bar| bar.started_at >= context.created_at)
                    .any(|bar| match context.side {
                        Side::Long => bar.high > context.upper,
                        Side::Short => bar.low < context.lower,
                    });
                let touched = approached
                    && (mark >= context.lower && mark <= context.upper
                        || bars
                            .s5
                            .back()
                            .copied()
                            .filter(|bar| bar.started_at >= context.created_at)
                            .is_some_and(|bar| zone_intersects(bar, &context)));
                if touched {
                    self.last_reason = "M15 first touch confirmed".to_string();
                    self.phase = MtfPhase::WaitingM1 {
                        context,
                        touched_at: timestamp,
                        first_eligible_m1: next_boundary(timestamp, Timeframe::M1),
                        deadline: next_boundary(timestamp, Timeframe::M1)
                            + config.m1_confirmation_bars * Timeframe::M1.seconds(),
                    };
                } else {
                    self.phase = MtfPhase::WaitingContextTouch {
                        context,
                        approached,
                    };
                }
            }
            MtfPhase::WaitingM1 {
                context,
                touched_at,
                first_eligible_m1,
                deadline,
            } => {
                if context_invalidated(&context, bars, mark, config.invalidation_buffer_pct) {
                    self.consume(&context, "M15 invalidated while waiting for M1");
                    return;
                }
                if timestamp > deadline {
                    self.consume(&context, "M1 confirmation expired");
                    return;
                }
                let quality = Quality {
                    min_gap_atr: config.m1_min_gap_atr,
                    min_displacement_atr: config.m1_min_displacement_atr,
                    min_body_ratio: config.m1_min_body_ratio,
                };
                let confirmation = detected_zones(bars, Timeframe::M1, now, quality)
                    .into_iter()
                    .find(|zone| {
                        zone.side == context.side
                            && zone.id.first_started_at >= first_eligible_m1
                            && zone.created_at <= deadline
                    });
                if let Some(confirmation) = confirmation {
                    self.last_reason = "aligned M1 FVG confirmed".to_string();
                    self.phase = MtfPhase::WaitingS5Candidate {
                        context,
                        confirmation: confirmation.clone(),
                        touched_at,
                        deadline: confirmation.created_at
                            + config.s5_inversion_bars * Timeframe::S5.seconds(),
                    };
                }
            }
            MtfPhase::WaitingS5Candidate {
                context,
                confirmation,
                touched_at,
                deadline,
            } => {
                if context_invalidated(&context, bars, mark, config.invalidation_buffer_pct) {
                    self.consume(&context, "M15 invalidated before S5 trigger");
                    return;
                }
                if timestamp > deadline {
                    self.consume(&context, "S5 candidate expired");
                    return;
                }
                let opposite = context.side.inverted();
                let trigger = detected_s5_zones(bars, now, config)
                    .into_iter()
                    .filter(|zone| {
                        zone.side == opposite
                            && zone.id.first_started_at >= confirmation.created_at
                            && zone.created_at <= deadline
                    })
                    .min_by(|a, b| {
                        let distance_a = if mark < a.lower {
                            a.lower - mark
                        } else if mark > a.upper {
                            mark - a.upper
                        } else {
                            0.0
                        };
                        let distance_b = if mark < b.lower {
                            b.lower - mark
                        } else if mark > b.upper {
                            mark - b.upper
                        } else {
                            0.0
                        };
                        distance_a.total_cmp(&distance_b)
                    });
                if let Some(trigger) = trigger {
                    self.last_reason = "opposing S5 FVG selected".to_string();
                    let deadline =
                        trigger.created_at + config.s5_inversion_bars * Timeframe::S5.seconds();
                    self.phase = MtfPhase::WaitingS5Inversion {
                        context,
                        confirmation,
                        trigger,
                        touched_at,
                        deadline,
                    };
                }
            }
            MtfPhase::WaitingS5Inversion {
                context,
                confirmation,
                trigger,
                touched_at,
                deadline,
            } => {
                if context_invalidated(&context, bars, mark, config.invalidation_buffer_pct) {
                    self.consume(&context, "M15 invalidated before S5 inversion");
                    return;
                }
                if timestamp > deadline {
                    self.consume(&context, "S5 inversion expired");
                    return;
                }
                let inversion = bars
                    .s5
                    .iter()
                    .copied()
                    .filter(|bar| {
                        bar.is_closed(now)
                            && bar.started_at >= trigger.created_at
                            && bar.started_at >= confirmation.created_at
                    })
                    .find(|bar| match context.side {
                        Side::Long => bar.close > trigger.upper,
                        Side::Short => bar.close < trigger.lower,
                    });
                if let Some(inversion) = inversion {
                    self.last_reason = "S5 FVG inverted".to_string();
                    self.phase = MtfPhase::WaitingS5Retest {
                        context,
                        confirmation,
                        trigger,
                        touched_at,
                        inverted_at: inversion.started_at,
                        deadline: inversion.started_at
                            + Timeframe::S5.seconds()
                            + config.s5_retest_bars * Timeframe::S5.seconds(),
                    };
                }
            }
            MtfPhase::WaitingS5Retest {
                context,
                confirmation,
                trigger,
                touched_at,
                inverted_at,
                deadline,
            } => {
                if context_invalidated(&context, bars, mark, config.invalidation_buffer_pct) {
                    self.consume(&context, "M15 invalidated before iFVG retest");
                    return;
                }
                if timestamp > deadline {
                    self.consume(&context, "S5 iFVG retest expired");
                    return;
                }
                let gap = trigger.upper - trigger.lower;
                let entry_level = match context.side {
                    Side::Long => trigger.upper - gap * config.s5_entry_depth,
                    Side::Short => trigger.lower + gap * config.s5_entry_depth,
                };
                let mut terminal = None;
                for bar in bars.s5.iter().copied().filter(|bar| {
                    bar.is_closed(now) && bar.started_at > inverted_at && bar.started_at <= deadline
                }) {
                    let invalidated = match context.side {
                        Side::Long => {
                            bar.low < trigger.lower * (1.0 - config.invalidation_buffer_pct)
                        }
                        Side::Short => {
                            bar.high > trigger.upper * (1.0 + config.invalidation_buffer_pct)
                        }
                    };
                    if invalidated {
                        terminal = Some(Err("S5 iFVG invalidated before retest"));
                        break;
                    }
                    let retested = match context.side {
                        Side::Long => bar.low <= entry_level && bar.close >= entry_level,
                        Side::Short => bar.high >= entry_level && bar.close <= entry_level,
                    };
                    if retested {
                        terminal = Some(Ok(bar));
                        break;
                    }
                }
                if let Some(Err(reason)) = terminal {
                    self.consume(&context, reason);
                } else if let Some(Ok(retest)) = terminal {
                    self.last_reason = "S5 iFVG retest confirmed".to_string();
                    self.phase = MtfPhase::Ready(MtfSetup {
                        side: context.side,
                        context,
                        confirmation,
                        trigger,
                        context_touched_at: touched_at,
                        inverted_at,
                        retested_at: retest.started_at,
                        entry_deadline: retest.started_at + 20,
                        entry_level,
                    });
                }
            }
            MtfPhase::Ready(setup) => {
                let trigger_invalidated = match setup.side {
                    Side::Long => {
                        mark < setup.trigger.lower * (1.0 - config.invalidation_buffer_pct)
                    }
                    Side::Short => {
                        mark > setup.trigger.upper * (1.0 + config.invalidation_buffer_pct)
                    }
                };
                if timestamp > setup.entry_deadline
                    || trigger_invalidated
                    || context_invalidated(
                        &setup.context,
                        bars,
                        mark,
                        config.invalidation_buffer_pct,
                    )
                {
                    self.consume(&setup.context, "entry-ready setup expired or invalidated");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(
        tf: Timeframe,
        started_at: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
    ) -> Candle {
        Candle {
            timeframe: tf,
            started_at,
            open,
            high,
            low,
            close,
            volume: 1.0,
            trades: 1,
        }
    }

    #[test]
    fn tick_updates_all_three_timeframes() {
        let mut bars = MtfBars::default();
        bars.update(&TradeTick {
            timestamp: 901.0,
            is_buy: true,
            price: 100.0,
            size: 2.0,
        });
        assert_eq!(bars.s5.back().unwrap().started_at, 900);
        assert_eq!(bars.m1.back().unwrap().started_at, 900);
        assert_eq!(bars.m15.back().unwrap().started_at, 900);
    }

    #[test]
    fn non_consecutive_candles_cannot_form_fvg() {
        let candles = vec![
            candle(Timeframe::M15, 0, 100.0, 101.0, 99.0, 100.0),
            candle(Timeframe::M15, 1800, 101.0, 105.0, 101.0, 104.0),
            candle(Timeframe::M15, 2700, 106.0, 107.0, 106.0, 107.0),
        ];
        assert!(detect_at(
            &candles,
            2,
            Quality {
                min_gap_atr: 0.0,
                min_displacement_atr: 0.0,
                min_body_ratio: 0.0,
            }
        )
        .is_none());
    }

    fn baseline(tf: Timeframe, start: i64, count: usize, price: f64) -> Vec<Candle> {
        (0..count)
            .map(|index| {
                let open = price + (index % 2) as f64 * 0.1;
                candle(
                    tf,
                    start + index as i64 * tf.seconds(),
                    open,
                    open + 0.5,
                    open - 0.5,
                    open + 0.1,
                )
            })
            .collect()
    }

    fn test_config() -> MtfConfig {
        MtfConfig {
            m15_min_gap_atr: 0.01,
            m15_min_displacement_atr: 0.5,
            m15_min_body_ratio: 0.5,
            m15_max_age_bars: 16,
            m1_min_gap_atr: 0.01,
            m1_min_displacement_atr: 0.5,
            m1_min_body_ratio: 0.5,
            m1_confirmation_bars: 15,
            s5_min_gap_pct: 0.0001,
            s5_min_body_ratio: 0.5,
            s5_inversion_bars: 24,
            s5_retest_bars: 12,
            s5_entry_depth: 0.5,
            invalidation_buffer_pct: 0.001,
        }
    }

    fn zone(timeframe: Timeframe, side: Side, lower: f64, upper: f64, created_at: i64) -> FvgZone {
        FvgZone {
            id: ZoneId {
                timeframe,
                first_started_at: created_at - timeframe.seconds() * 3,
                third_started_at: created_at - timeframe.seconds(),
                side,
            },
            side,
            lower,
            upper,
            created_at,
            middle_range_atr: 1.0,
            middle_body_ratio: 0.8,
            gap_atr: 0.2,
        }
    }

    #[test]
    fn bullish_cascade_requires_touch_confirmation_inversion_and_later_retest() {
        let mut bars = MtfBars::default();
        let mut m15 = baseline(Timeframe::M15, 0, 14, 100.0);
        m15.extend([
            candle(Timeframe::M15, 12_600, 100.0, 101.0, 99.5, 100.5),
            candle(Timeframe::M15, 13_500, 100.5, 105.0, 100.3, 104.5),
            candle(Timeframe::M15, 14_400, 102.0, 104.0, 102.0, 103.5),
        ]);
        bars.seed(Timeframe::M15, m15);

        let mut tracker = MtfFvgTracker::default();
        let config = test_config();
        tracker.advance(&bars, 15_301.0, 103.0, &config);
        assert_eq!(tracker.phase_name(), "WAIT_M15_TOUCH");
        tracker.advance(&bars, 15_302.0, 101.5, &config);
        assert_eq!(tracker.phase_name(), "WAIT_M1_CONFIRMATION");

        let mut m1 = baseline(Timeframe::M1, 14_520, 14, 101.0);
        m1.extend([
            candle(Timeframe::M1, 15_360, 101.0, 101.2, 100.8, 101.0),
            candle(Timeframe::M1, 15_420, 101.0, 104.0, 100.9, 103.8),
            candle(Timeframe::M1, 15_480, 102.0, 103.5, 102.0, 103.0),
        ]);
        bars.seed(Timeframe::M1, m1);
        tracker.advance(&bars, 15_541.0, 103.0, &config);
        assert_eq!(tracker.phase_name(), "WAIT_S5_FVG");

        bars.seed(
            Timeframe::S5,
            [
                candle(Timeframe::S5, 15_540, 104.0, 105.0, 103.5, 104.0),
                candle(Timeframe::S5, 15_545, 103.5, 103.8, 101.8, 102.0),
                candle(Timeframe::S5, 15_550, 102.2, 102.5, 101.5, 102.0),
            ],
        );
        tracker.advance(&bars, 15_556.0, 102.8, &config);
        assert_eq!(tracker.phase_name(), "WAIT_S5_INVERSION");

        bars.seed(
            Timeframe::S5,
            [candle(Timeframe::S5, 15_555, 102.8, 104.2, 102.7, 104.0)],
        );
        tracker.advance(&bars, 15_561.0, 104.0, &config);
        assert_eq!(tracker.phase_name(), "WAIT_S5_RETEST");
        assert!(tracker.ready().is_none());

        bars.seed(
            Timeframe::S5,
            [candle(Timeframe::S5, 15_560, 103.8, 104.0, 102.9, 103.2)],
        );
        tracker.advance(&bars, 15_566.0, 103.2, &config);
        assert_eq!(tracker.phase_name(), "ENTRY_READY");
        assert_eq!(tracker.ready().unwrap().side, Side::Long);
    }

    #[test]
    fn bearish_cascade_reaches_entry_only_after_bullish_s5_fvg_fails() {
        let mut bars = MtfBars::default();
        let mut m15 = baseline(Timeframe::M15, 0, 14, 100.0);
        m15.extend([
            candle(Timeframe::M15, 12_600, 103.0, 104.0, 102.0, 103.0),
            candle(Timeframe::M15, 13_500, 103.0, 103.2, 98.0, 98.5),
            candle(Timeframe::M15, 14_400, 100.5, 101.0, 99.0, 99.5),
        ]);
        bars.seed(Timeframe::M15, m15);
        let mut tracker = MtfFvgTracker::default();
        let config = test_config();
        tracker.advance(&bars, 15_301.0, 99.5, &config);
        tracker.advance(&bars, 15_302.0, 101.5, &config);
        assert_eq!(tracker.phase_name(), "WAIT_M1_CONFIRMATION");

        let mut m1 = baseline(Timeframe::M1, 14_520, 14, 101.0);
        m1.extend([
            candle(Timeframe::M1, 15_360, 102.0, 102.2, 101.8, 102.0),
            candle(Timeframe::M1, 15_420, 102.0, 102.1, 98.5, 98.8),
            candle(Timeframe::M1, 15_480, 100.5, 100.8, 99.0, 99.5),
        ]);
        bars.seed(Timeframe::M1, m1);
        tracker.advance(&bars, 15_541.0, 99.5, &config);
        assert_eq!(tracker.phase_name(), "WAIT_S5_FVG");

        bars.seed(
            Timeframe::S5,
            [
                candle(Timeframe::S5, 15_540, 99.0, 99.5, 98.5, 99.0),
                candle(Timeframe::S5, 15_545, 99.2, 101.2, 99.0, 101.0),
                candle(Timeframe::S5, 15_550, 100.5, 101.5, 100.5, 101.0),
            ],
        );
        tracker.advance(&bars, 15_556.0, 100.0, &config);
        assert_eq!(tracker.phase_name(), "WAIT_S5_INVERSION");
        bars.seed(
            Timeframe::S5,
            [candle(Timeframe::S5, 15_555, 100.0, 100.1, 98.8, 99.0)],
        );
        tracker.advance(&bars, 15_561.0, 99.0, &config);
        assert_eq!(tracker.phase_name(), "WAIT_S5_RETEST");
        bars.seed(
            Timeframe::S5,
            [candle(Timeframe::S5, 15_560, 99.2, 100.4, 99.0, 100.0)],
        );
        tracker.advance(&bars, 15_566.0, 100.0, &config);
        assert_eq!(tracker.phase_name(), "ENTRY_READY");
        assert_eq!(tracker.ready().unwrap().side, Side::Short);
    }

    #[test]
    fn s5_candidate_formed_before_m1_confirmation_is_rejected() {
        let context = zone(Timeframe::M15, Side::Long, 90.0, 95.0, 0);
        let confirmation = zone(Timeframe::M1, Side::Long, 96.0, 97.0, 100);
        let mut tracker = MtfFvgTracker {
            phase: MtfPhase::WaitingS5Candidate {
                context,
                confirmation,
                touched_at: 60,
                deadline: 220,
            },
            consumed_contexts: VecDeque::new(),
            last_reason: String::new(),
        };
        let mut bars = MtfBars::default();
        bars.seed(
            Timeframe::S5,
            [
                candle(Timeframe::S5, 80, 102.0, 103.0, 101.0, 102.0),
                candle(Timeframe::S5, 85, 102.0, 102.2, 99.0, 99.5),
                candle(Timeframe::S5, 90, 99.0, 99.5, 98.0, 99.0),
            ],
        );
        tracker.advance(&bars, 111.0, 99.0, &test_config());
        assert_eq!(tracker.phase_name(), "WAIT_S5_FVG");
    }

    #[test]
    fn far_edge_breach_wins_over_a_later_valid_retest() {
        let context = zone(Timeframe::M15, Side::Long, 90.0, 95.0, 0);
        let confirmation = zone(Timeframe::M1, Side::Long, 96.0, 97.0, 100);
        let trigger = zone(Timeframe::S5, Side::Short, 100.0, 101.0, 120);
        let mut tracker = MtfFvgTracker {
            phase: MtfPhase::WaitingS5Retest {
                context,
                confirmation,
                trigger,
                touched_at: 60,
                inverted_at: 125,
                deadline: 180,
            },
            consumed_contexts: VecDeque::new(),
            last_reason: String::new(),
        };
        let mut bars = MtfBars::default();
        bars.seed(
            Timeframe::S5,
            [
                candle(Timeframe::S5, 130, 101.5, 101.6, 99.0, 101.2),
                candle(Timeframe::S5, 135, 101.3, 101.4, 100.4, 100.8),
            ],
        );
        tracker.advance(&bars, 141.0, 101.0, &test_config());
        assert_eq!(tracker.phase_name(), "WAIT_M15_CONTEXT");
        assert!(tracker.last_reason.contains("invalidated"));
    }
}
