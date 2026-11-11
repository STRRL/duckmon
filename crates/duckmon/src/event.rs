use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use duckdb::arrow::array::{
    ArrayRef, Date32Array, Int64Array, StringArray, TimestampNanosecondArray,
};
use duckdb::arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use duckdb::arrow::record_batch::RecordBatch;

use crate::{Error, Result};

pub const NANOS_PER_DAY: i64 = 86_400_000_000_000;

/// One telemetry event: a log, a span, a metric snapshot or a plain event.
/// See `docs/contract.md` for how each kind is stored.
#[derive(Debug, Clone)]
pub struct Event {
    /// When the event happened. For a span, the start time.
    pub ts: SystemTime,
    /// Stable, low cardinality event name in lowercase dotted words, such as `http.request`.
    pub name: String,
    /// Human readable log text. Set only for logs.
    pub message: Option<String>,
    /// Span length. Set only for spans.
    pub duration: Option<Duration>,
    /// 32 lowercase hex characters.
    pub trace_id: Option<String>,
    /// 16 lowercase hex characters.
    pub span_id: Option<String>,
    /// Parent span ID of a span. None for a root span.
    pub parent_id: Option<String>,
    /// Empty map is stored as NULL.
    pub attrs: serde_json::Map<String, serde_json::Value>,
    /// Replaces the store's resource for this event when set. An empty map is stored as NULL.
    pub resource: Option<serde_json::Map<String, serde_json::Value>>,
}

impl Event {
    /// Creates an event with the given name, the current time, and no other fields.
    pub fn new(name: impl Into<String>) -> Event {
        Event {
            ts: SystemTime::now(),
            name: name.into(),
            message: None,
            duration: None,
            trace_id: None,
            span_id: None,
            parent_id: None,
            attrs: serde_json::Map::new(),
            resource: None,
        }
    }
}

/// Nanoseconds since 1970 UTC, or None when the time is before 1970 or does not fit in i64.
pub fn nanos_since_epoch(time: SystemTime) -> Option<i64> {
    let since_epoch = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since_epoch.as_nanos()).ok()
}

/// Days since 1970 UTC for a time given as nanoseconds since 1970.
pub fn date_from_nanos(nanos: i64) -> i32 {
    // The result always fits: i64::MAX nanoseconds is about 106 751 days.
    nanos.div_euclid(NANOS_PER_DAY) as i32
}

/// Builds one record batch in the column order of the buffer table.
pub fn events_to_record_batch(
    events: &[Event],
    resource: &serde_json::Map<String, serde_json::Value>,
) -> Result<RecordBatch> {
    let resource_text = if resource.is_empty() {
        None
    } else {
        Some(json_text(resource)?)
    };

    let mut ts_values = Vec::with_capacity(events.len());
    let mut duration_values = Vec::with_capacity(events.len());
    let mut attrs_values = Vec::with_capacity(events.len());
    let mut resource_values = Vec::with_capacity(events.len());
    let mut date_values = Vec::with_capacity(events.len());
    for event in events {
        let nanos = nanos_since_epoch(event.ts).ok_or_else(|| {
            Error::InvalidEvent(format!(
                "event {:?} has a time before 1970 or too far in the future",
                event.name
            ))
        })?;
        let duration_ns = match event.duration {
            Some(duration) => Some(i64::try_from(duration.as_nanos()).map_err(|_| {
                Error::InvalidEvent(format!("event {:?} has a duration that is too long", event.name))
            })?),
            None => None,
        };
        let attrs_text = if event.attrs.is_empty() {
            None
        } else {
            Some(json_text(&event.attrs)?)
        };
        let event_resource_text = match &event.resource {
            None => resource_text.clone(),
            Some(map) if map.is_empty() => None,
            Some(map) => Some(json_text(map)?),
        };
        ts_values.push(nanos);
        duration_values.push(duration_ns);
        attrs_values.push(attrs_text);
        resource_values.push(event_resource_text);
        date_values.push(date_from_nanos(nanos));
    }

    let names: Vec<&str> = events.iter().map(|e| e.name.as_str()).collect();
    let messages: Vec<Option<&str>> = events.iter().map(|e| e.message.as_deref()).collect();
    let trace_ids: Vec<Option<&str>> = events.iter().map(|e| e.trace_id.as_deref()).collect();
    let span_ids: Vec<Option<&str>> = events.iter().map(|e| e.span_id.as_deref()).collect();
    let parent_ids: Vec<Option<&str>> = events.iter().map(|e| e.parent_id.as_deref()).collect();

    let schema = Schema::new(vec![
        Field::new("ts", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("name", DataType::Utf8, false),
        Field::new("message", DataType::Utf8, true),
        Field::new("duration_ns", DataType::Int64, true),
        Field::new("trace_id", DataType::Utf8, true),
        Field::new("span_id", DataType::Utf8, true),
        Field::new("parent_id", DataType::Utf8, true),
        Field::new("resource", DataType::Utf8, true),
        Field::new("attrs", DataType::Utf8, true),
        Field::new("date", DataType::Date32, false),
    ]);
    let columns: Vec<ArrayRef> = vec![
        Arc::new(TimestampNanosecondArray::from(ts_values)),
        Arc::new(StringArray::from(names)),
        Arc::new(StringArray::from(messages)),
        Arc::new(Int64Array::from(duration_values)),
        Arc::new(StringArray::from(trace_ids)),
        Arc::new(StringArray::from(span_ids)),
        Arc::new(StringArray::from(parent_ids)),
        Arc::new(StringArray::from(resource_values)),
        Arc::new(StringArray::from(attrs_values)),
        Arc::new(Date32Array::from(date_values)),
    ];
    RecordBatch::try_new(Arc::new(schema), columns)
        .map_err(|err| Error::InvalidEvent(format!("could not build record batch: {err}")))
}

fn json_text(map: &serde_json::Map<String, serde_json::Value>) -> Result<String> {
    serde_json::to_string(map)
        .map_err(|err| Error::InvalidEvent(format!("could not write JSON: {err}")))
}
