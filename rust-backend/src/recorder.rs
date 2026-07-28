use crate::config::SETTINGS;
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static SENDER: OnceLock<SyncSender<String>> = OnceLock::new();

pub fn init() {
    if !SETTINGS.record_raw_events || SENDER.get().is_some() {
        return;
    }
    let (sender, receiver) = sync_channel::<String>(8_192);
    if SENDER.set(sender).is_err() {
        return;
    }
    let directory = PathBuf::from(&SETTINGS.raw_events_dir);
    let segment_limit = SETTINGS.raw_segment_bytes.max(1_048_576);
    let disk_budget = SETTINGS.raw_disk_budget_bytes.max(segment_limit);
    std::thread::Builder::new()
        .name("pulsebook-raw-recorder".into())
        .spawn(move || {
            if fs::create_dir_all(&directory).is_err() {
                return;
            }
            let mut writer = open_segment(&directory);
            let mut written = 0_u64;
            while let Ok(line) = receiver.recv() {
                if written + line.len() as u64 + 1 > segment_limit {
                    writer = open_segment(&directory);
                    written = 0;
                    enforce_budget(&directory, disk_budget);
                }
                if let Some(file) = writer.as_mut() {
                    if writeln!(file, "{line}").is_ok() {
                        written += line.len() as u64 + 1;
                    }
                }
            }
        })
        .ok();
}

pub fn record(kind: &str, symbol: &str, exchange_ts: f64, local_ts: f64, sequence: i64, payload: &Value) -> bool {
    let Some(sender) = SENDER.get() else { return true; };
    let line = json!({
        "kind": kind,
        "symbol": symbol,
        "exchange_timestamp": exchange_ts,
        "local_receive_timestamp": local_ts,
        "sequence": sequence,
        "payload": payload,
    }).to_string();
    match sender.try_send(line) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
    }
}

fn open_segment(directory: &Path) -> Option<BufWriter<File>> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis();
    let path = directory.join(format!("pulsebook-{timestamp}.jsonl"));
    OpenOptions::new().create(true).append(true).open(path).ok().map(BufWriter::new)
}

fn enforce_budget(directory: &Path, budget: u64) {
    let Ok(entries) = fs::read_dir(directory) else { return; };
    let mut files: Vec<(PathBuf, u64)> = entries.filter_map(Result::ok).filter_map(|entry| {
        let metadata = entry.metadata().ok()?;
        metadata.is_file().then_some((entry.path(), metadata.len()))
    }).collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut total: u64 = files.iter().map(|(_, size)| *size).sum();
    for (path, size) in files {
        if total <= budget { break; }
        if fs::remove_file(path).is_ok() { total = total.saturating_sub(size); }
    }
}
