//! Deterministic, event-time replay with explicit walk-forward folds.
#![allow(dead_code)]

use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ReplayEvent {
    pub timestamp: f64,
    pub sequence: i64,
    pub symbol: String,
    pub kind: String,
    pub payload: Value,
}

pub struct ReplayEngine<F: FnMut(&ReplayEvent)> {
    handler: F,
    pub now: f64,
    last_sequence: HashMap<String, i64>,
    pub data_quality_errors: u64,
}

pub type WalkForwardFold = (Vec<ReplayEvent>, Vec<ReplayEvent>);

impl<F: FnMut(&ReplayEvent)> ReplayEngine<F> {
    pub fn new(handler: F) -> Self {
        Self {
            handler,
            now: 0.0,
            last_sequence: HashMap::new(),
            data_quality_errors: 0,
        }
    }

    pub fn run(&mut self, events: Vec<ReplayEvent>) {
        let mut ordered = events;
        ordered.sort_by(|a, b| {
            a.timestamp
                .total_cmp(&b.timestamp)
                .then(a.sequence.cmp(&b.sequence))
        });
        for event in &ordered {
            // Event time is advanced before dispatch: the handler can only
            // see this event and previous events, never future rows.
            let previous = self.last_sequence.get(&event.symbol).copied().unwrap_or(-1);
            if event.sequence <= previous || event.timestamp < self.now {
                self.data_quality_errors += 1;
                continue;
            }
            self.now = event.timestamp;
            self.last_sequence
                .insert(event.symbol.clone(), event.sequence);
            (self.handler)(event);
        }
    }
}

/// Yields expanding train windows and untouched chronological test folds.
pub fn walk_forward(events: &[ReplayEvent], folds: usize) -> Result<Vec<WalkForwardFold>, String> {
    let mut rows: Vec<ReplayEvent> = events.to_vec();
    rows.sort_by(|a, b| {
        a.timestamp
            .total_cmp(&b.timestamp)
            .then(a.sequence.cmp(&b.sequence))
    });
    if folds < 2 || rows.len() < folds {
        return Err("walk-forward requires at least two non-empty folds".into());
    }
    let size = rows.len() / folds;
    let mut result = Vec::new();
    for index in 1..folds {
        let train_end = size * index;
        let test_end = if index == folds - 1 {
            rows.len()
        } else {
            size * (index + 1)
        };
        result.push((
            rows[..train_end].to_vec(),
            rows[train_end..test_end].to_vec(),
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(ts: f64, seq: i64, symbol: &str) -> ReplayEvent {
        ReplayEvent {
            timestamp: ts,
            sequence: seq,
            symbol: symbol.into(),
            kind: "book".into(),
            payload: json!({}),
        }
    }

    #[test]
    fn replay_never_goes_backwards_in_time() {
        let mut seen = Vec::new();
        {
            let mut engine = ReplayEngine::new(|e: &ReplayEvent| seen.push(e.timestamp));
            engine.run(vec![
                event(3.0, 3, "A"),
                event(1.0, 1, "A"),
                event(2.0, 2, "A"),
            ]);
            assert_eq!(engine.data_quality_errors, 0);
        }
        assert_eq!(seen, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn stale_sequences_count_as_data_quality_errors() {
        let mut count = 0;
        let mut engine = ReplayEngine::new(|_| count += 1);
        engine.run(vec![
            event(1.0, 5, "A"),
            event(2.0, 4, "A"),
            event(3.0, 6, "A"),
        ]);
        assert_eq!(engine.data_quality_errors, 1);
        assert_eq!(count, 2);
    }

    #[test]
    fn walk_forward_folds_never_overlap() {
        let events: Vec<ReplayEvent> = (0..40).map(|i| event(i as f64, i as i64, "A")).collect();
        let folds = walk_forward(&events, 4).unwrap();
        assert_eq!(folds.len(), 3);
        for (train, test) in &folds {
            let train_max = train.iter().map(|e| e.timestamp).fold(f64::MIN, f64::max);
            let test_min = test.iter().map(|e| e.timestamp).fold(f64::MAX, f64::min);
            assert!(
                train_max < test_min,
                "test folds must be strictly after training data"
            );
        }
    }

    #[test]
    fn walk_forward_rejects_tiny_samples() {
        let events: Vec<ReplayEvent> = (0..3).map(|i| event(i as f64, i as i64, "A")).collect();
        assert!(walk_forward(&events, 4).is_err());
    }
}
