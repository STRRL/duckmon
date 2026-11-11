use std::time::{Duration, UNIX_EPOCH};

use duckmon::{CompactAction, CompactOptions, CompactReport, Event, Options};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// One line of the events text. The keys are the column names.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventLine {
    ts: u64,
    name: String,
    message: Option<String>,
    duration_ns: Option<u64>,
    trace_id: Option<String>,
    span_id: Option<String>,
    parent_id: Option<String>,
    attrs: Option<Map<String, Value>>,
}

/// The options JSON of `duckmon_open`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct OpenOptionsJSON {
    flush_interval_ms: Option<u64>,
    channel_capacity: Option<usize>,
    resource: Option<Map<String, Value>>,
}

/// The options JSON of `duckmon_compact`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CompactOptionsJSON {
    keep_days: Option<u32>,
    settle_time_ms: Option<u64>,
    dry_run: Option<bool>,
}

/// Parses JSON Lines text into events. Empty lines are skipped. The error
/// names the first wrong line, counted from 1.
pub fn events_from_json_lines(text: &str) -> Result<Vec<Event>, String> {
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event = event_from_line(line).map_err(|reason| format!("line {}: {reason}", index + 1))?;
        events.push(event);
    }
    Ok(events)
}

fn event_from_line(line: &str) -> Result<Event, String> {
    let line: EventLine = serde_json::from_str(line).map_err(|err| reason_with_column(&err))?;
    if line.name.is_empty() {
        return Err("name must not be empty".to_string());
    }
    let largest = i64::MAX as u64;
    if line.ts > largest {
        return Err(format!("ts {} is above the largest time, {largest}", line.ts));
    }
    if line.duration_ns.is_some_and(|duration_ns| duration_ns > largest) {
        return Err(format!("duration_ns is above the largest length, {largest}"));
    }
    Ok(Event {
        ts: UNIX_EPOCH + Duration::from_nanos(line.ts),
        name: line.name,
        message: line.message,
        duration: line.duration_ns.map(Duration::from_nanos),
        trace_id: line.trace_id,
        span_id: line.span_id,
        parent_id: line.parent_id,
        attrs: line.attrs.unwrap_or_default(),
    })
}

/// The text of a JSON error for one line, which names the column but not the
/// line, because the caller names the line in the whole text.
fn reason_with_column(err: &serde_json::Error) -> String {
    let text = err.to_string();
    let position = format!(" at line {} column {}", err.line(), err.column());
    match text.strip_suffix(&position) {
        Some(reason) => format!("{reason} (column {})", err.column()),
        None => text,
    }
}

/// Parses the options of `duckmon_open`. `None` gives the defaults.
pub fn options_from_json(text: Option<&str>) -> Result<Options, String> {
    // The channel reserves its slots up front, so a huge capacity would fail or take very long.
    const MAX_CHANNEL_CAPACITY: usize = 1_000_000;
    let fields: OpenOptionsJSON = parse_options(text)?;
    let defaults = Options::default();
    let channel_capacity = fields.channel_capacity.unwrap_or(defaults.channel_capacity);
    if !(1..=MAX_CHANNEL_CAPACITY).contains(&channel_capacity) {
        return Err(format!(
            "options: channel_capacity must be from 1 to {MAX_CHANNEL_CAPACITY}, got {channel_capacity}"
        ));
    }
    Ok(Options {
        flush_interval: fields
            .flush_interval_ms
            .map_or(defaults.flush_interval, Duration::from_millis),
        channel_capacity,
        resource: fields.resource.unwrap_or(defaults.resource),
    })
}

/// Parses the options of `duckmon_compact`. `None` gives the defaults.
pub fn compact_options_from_json(text: Option<&str>) -> Result<CompactOptions, String> {
    let fields: CompactOptionsJSON = parse_options(text)?;
    let defaults = CompactOptions::default();
    Ok(CompactOptions {
        keep_days: fields.keep_days,
        settle_time: fields
            .settle_time_ms
            .map_or(defaults.settle_time, Duration::from_millis),
        dry_run: fields.dry_run.unwrap_or(defaults.dry_run),
    })
}

fn parse_options<T: Default + for<'de> Deserialize<'de>>(text: Option<&str>) -> Result<T, String> {
    match text {
        None => Ok(T::default()),
        Some(text) => serde_json::from_str(text).map_err(|err| format!("options: {err}")),
    }
}

/// The compact report as JSON text: one object per action, in order.
pub fn compact_report_json(report: &CompactReport) -> String {
    let actions: Vec<Value> = report.actions.iter().map(action_json).collect();
    json!({ "actions": actions }).to_string()
}

fn action_json(action: &CompactAction) -> Value {
    match action {
        CompactAction::RemoveMergedInput { path, merged_into } => json!({
            "action": "remove_merged_input",
            "path": path.to_string_lossy(),
            "merged_into": merged_into.to_string_lossy(),
        }),
        CompactAction::RemoveDeletedDay { path } => json!({
            "action": "remove_deleted_day",
            "path": path.to_string_lossy(),
        }),
        CompactAction::DeleteDay { day, files, bytes } => json!({
            "action": "delete_day",
            "day": day,
            "files": files,
            "bytes": bytes,
        }),
        CompactAction::RemoveTempFile { path } => json!({
            "action": "remove_temp_file",
            "path": path.to_string_lossy(),
        }),
        CompactAction::MergeFiles {
            day,
            inputs,
            input_bytes,
            rows,
            output,
        } => json!({
            "action": "merge_files",
            "day": day,
            "inputs": inputs,
            "input_bytes": input_bytes,
            "rows": rows,
            "output": output.to_string_lossy(),
        }),
        CompactAction::SkipDay { day, reason } => json!({
            "action": "skip_day",
            "day": day,
            "reason": reason,
        }),
    }
}
