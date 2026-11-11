use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use duckmon::{Event, Options};
use serde_json::{Map, Value};

use crate::Failure;

/// Arguments of `duckmon emit`.
#[derive(Debug, clap::Args)]
pub struct EmitArgs {
    /// Event name in lowercase dotted words, such as `deploy.finished`.
    pub name: String,
    /// Attributes. `key=value` stores the text as a JSON string. `key:=value`
    /// parses the value as JSON: a number, true, false, null, an array or an object.
    #[arg(value_name = "ATTR", value_parser = parse_attr)]
    pub attrs: Vec<(String, Value)>,
    /// Log text.
    #[arg(short, long)]
    pub message: Option<String>,
    /// Span length: a whole number with a unit, one of ns, us, ms, s, m, h. Example: 1500ms.
    #[arg(long, value_parser = parse_span_length)]
    pub duration: Option<Duration>,
    /// When the event happened. Default is now. Either a whole number with a unit,
    /// meaning time since 1970 (such as 1762761600s), or RFC 3339 (such as 2026-11-10T08:15:30.5Z).
    #[arg(long, value_parser = parse_time)]
    pub ts: Option<SystemTime>,
    /// Trace ID, 32 hex characters.
    #[arg(long, value_parser = |text: &str| parse_hex_id(text, 32))]
    pub trace_id: Option<String>,
    /// Span ID, 16 hex characters.
    #[arg(long, value_parser = |text: &str| parse_hex_id(text, 16))]
    pub span_id: Option<String>,
    /// Parent span ID, 16 hex characters.
    #[arg(long, value_parser = |text: &str| parse_hex_id(text, 16))]
    pub parent_id: Option<String>,
    /// Resource attribute, same syntax as ATTR. May be given more than once.
    #[arg(long, value_name = "ATTR", value_parser = parse_attr)]
    pub resource: Vec<(String, Value)>,
}

/// Writes one event to the store and waits until it is in a Parquet file.
pub fn run(dir: &Path, args: EmitArgs) -> Result<(), Failure> {
    if args.name.is_empty() {
        return Err(Failure::BadInput("the event name must not be empty".to_string()));
    }
    let mut event = Event::new(args.name);
    if let Some(ts) = args.ts {
        event.ts = ts;
    }
    event.message = args.message;
    event.duration = args.duration;
    event.trace_id = args.trace_id;
    event.span_id = args.span_id;
    event.parent_id = args.parent_id;
    event.attrs = args.attrs.into_iter().collect();
    let resource: Map<String, Value> = args.resource.into_iter().collect();

    let store = duckmon::open(
        dir,
        Options {
            resource,
            ..Options::default()
        },
    )?;
    store.write_events(&[event])?;
    store.close()?;
    Ok(())
}

/// Parses `key=value` into a JSON string and `key:=value` into the JSON value it holds.
pub fn parse_attr(text: &str) -> Result<(String, Value), String> {
    let Some((key, value)) = text.split_once('=') else {
        return Err("expected key=value or key:=value".to_string());
    };
    let (key, value) = match key.strip_suffix(':') {
        Some(key) => {
            let value = serde_json::from_str(value).map_err(|err| format!("value of {key:?} is not JSON: {err}"))?;
            (key, value)
        }
        None => (key, Value::String(value.to_string())),
    };
    if key.is_empty() {
        return Err("the key must not be empty".to_string());
    }
    Ok((key.to_string(), value))
}

/// Parses a whole number followed by a unit: ns, us, ms, s, m or h.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let unit_start = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(unit_start);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("expected a whole number with a unit (ns, us, ms, s, m, h), got {text:?}"))?;
    let too_long = || format!("{text:?} is too long");
    match unit {
        "ns" => Ok(Duration::from_nanos(number)),
        "us" => Ok(Duration::from_micros(number)),
        "ms" => Ok(Duration::from_millis(number)),
        "s" => Ok(Duration::from_secs(number)),
        "m" => Ok(Duration::from_secs(number.checked_mul(60).ok_or_else(too_long)?)),
        "h" => Ok(Duration::from_secs(number.checked_mul(60 * 60).ok_or_else(too_long)?)),
        "" => Err(format!("{text:?} has no unit; add one of ns, us, ms, s, m, h")),
        _ => Err(format!("unknown unit {unit:?}; use one of ns, us, ms, s, m, h")),
    }
}

/// Parses a span length like `parse_duration`. The store keeps it as i64
/// nanoseconds, so it can be at most about 292 years.
pub fn parse_span_length(text: &str) -> Result<Duration, String> {
    let duration = parse_duration(text)?;
    if i64::try_from(duration.as_nanos()).is_err() {
        return Err(format!("{text:?} is too long; the longest span is about 292 years"));
    }
    Ok(duration)
}

/// Parses a time since 1970 with a unit, such as `1762761600s`, or an RFC 3339 time.
/// The store keeps times as i64 nanoseconds since 1970, so the latest time is in the year 2262.
pub fn parse_time(text: &str) -> Result<SystemTime, String> {
    let wrong_form = || {
        format!(
            "expected a whole number with a unit, meaning time since 1970, such as 1762761600s, \
             or RFC 3339, such as 2026-11-10T08:15:30Z, got {text:?}"
        )
    };
    let too_late = || format!("{text:?} is too far in the future; the latest time is in the year 2262");
    // RFC 3339 always holds `-` and `:`, so a digit followed by only letters and digits is a number with a unit.
    if text.starts_with(|c: char| c.is_ascii_digit()) && text.chars().all(|c| c.is_ascii_alphanumeric()) {
        let since_epoch = parse_duration(text).map_err(|_| wrong_form())?;
        if i64::try_from(since_epoch.as_nanos()).is_err() {
            return Err(too_late());
        }
        return Ok(UNIX_EPOCH + since_epoch);
    }
    let time = DateTime::parse_from_rfc3339(text).map_err(|_| wrong_form())?;
    if time.timestamp() < 0 {
        return Err(format!("{text:?} is before 1970"));
    }
    if time.timestamp_nanos_opt().is_none() {
        return Err(too_late());
    }
    Ok(SystemTime::from(time))
}

/// Checks that `text` is `length` hex characters and returns it in lowercase.
pub fn parse_hex_id(text: &str, length: usize) -> Result<String, String> {
    let id = text.to_ascii_lowercase();
    if id.len() != length || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("expected {length} hex characters, got {text:?}"));
    }
    Ok(id)
}
