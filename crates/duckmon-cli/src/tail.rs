use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{Int64Type, TimestampNanosecondType};
use chrono::DateTime;
use duckmon::{COMPACTED_FROM_KEY, Options, Store, data_files, file_list_literal, read_events_sql, sql_string_literal};

use crate::Failure;
use crate::emit::parse_duration;
use crate::output::{TIMESTAMP_FORMAT, format_json};

/// How `tail` prints each event.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum TailFormat {
    /// One readable line per event.
    Text,
    /// One JSON object per event, with every column.
    Json,
}

/// Arguments of `duckmon tail`.
#[derive(Debug, clap::Args)]
pub struct TailArgs {
    /// SQL condition on the columns of `events`, such as "name = 'http.request'".
    #[arg(long = "where", value_name = "SQL")]
    pub filter: Option<String>,
    #[arg(long, value_enum, default_value_t = TailFormat::Text)]
    pub format: TailFormat,
    /// How often to look for new files: a whole number with a unit, at least 100ms.
    /// It must stay below the settle time of `compact` (1h by default), or events can be missed.
    #[arg(long, default_value = "1s", value_parser = parse_interval)]
    pub interval: Duration,
}

/// Prints events from data files that appear after it starts, until stopped.
/// Each new file is printed once. Files made by `compact` are skipped,
/// because their rows were in older files.
///
/// Reading can fail for a moment while `compact` replaces files. Such a
/// failure never stops tail: files are marked as seen only after their rows
/// are printed, so unread files are simply read again at the next poll.
pub fn run(dir: &Path, args: &TailArgs) -> Result<(), Failure> {
    let store = duckmon::open(dir, Options::default())?;
    let filter = args.filter.as_deref().unwrap_or("true");
    // Files are listed before the filter check, which can take a while on a
    // large store, so that files written during the check are printed too.
    let mut seen: HashSet<PathBuf> = data_files(dir)?.into_iter().collect();
    check_filter(&store, dir, filter, args.interval)?;
    let mut stdout = std::io::stdout();
    loop {
        std::thread::sleep(args.interval);
        let current: HashSet<PathBuf> = data_files(dir)?.into_iter().collect();
        // Names are never reused, so forgetting removed files is safe and keeps memory bounded.
        seen.retain(|path| current.contains(path));
        let unread: Vec<PathBuf> = current.iter().filter(|path| !seen.contains(*path)).cloned().collect();
        if unread.is_empty() {
            continue;
        }
        let batches = match read_new_files(&store, dir, &unread, filter) {
            Ok(batches) => batches,
            Err(err) => {
                eprintln!("warning: could not read new files, trying again at the next poll: {err}");
                continue;
            }
        };
        let text = match args.format {
            TailFormat::Text => {
                let mut text = String::new();
                for batch in &batches {
                    for row in 0..batch.num_rows() {
                        text.push_str(&event_line(batch, row));
                        text.push('\n');
                    }
                }
                text.into_bytes()
            }
            TailFormat::Json => format_json(&batches).map_err(|err| Failure::Runtime(err.to_string()))?,
        };
        stdout.write_all(&text)?;
        stdout.flush()?;
        seen.extend(unread);
    }
}

/// Fails when `filter` is not valid SQL. The check reads `events`, so it can
/// also fail because `compact` removed a file at that moment. That case is
/// known by a data file that existed before the check being gone after it,
/// and the check is then tried again.
fn check_filter(store: &Store, dir: &Path, filter: &str, interval: Duration) -> Result<(), Failure> {
    loop {
        let before = data_files(dir)?;
        // The filter sits on its own lines so a trailing `-- comment` cannot hide the closing parenthesis.
        let Err(err) = store.query(&format!("SELECT * FROM events WHERE (\n{filter}\n) LIMIT 0")) else {
            return Ok(());
        };
        let after: HashSet<PathBuf> = data_files(dir)?.into_iter().collect();
        if before.iter().all(|path| after.contains(path)) {
            return Err(err.into());
        }
        std::thread::sleep(interval);
    }
}

/// Reads the matching rows of new files. When the read fails, files that
/// were removed in the meantime are reported on stderr and left out, and the
/// read is tried once more.
fn read_new_files(store: &Store, dir: &Path, files: &[PathBuf], filter: &str) -> duckmon::Result<Vec<RecordBatch>> {
    if let Ok(batches) = query_new_files(store, files, filter) {
        return Ok(batches);
    }
    let present: HashSet<PathBuf> = data_files(dir)?.into_iter().collect();
    let mut kept = Vec::new();
    for path in files {
        if present.contains(path) {
            kept.push(path.clone());
        } else {
            eprintln!("warning: {} was removed before tail read it", path.display());
        }
    }
    if kept.is_empty() {
        return Ok(Vec::new());
    }
    query_new_files(store, &kept, filter)
}

fn query_new_files(store: &Store, files: &[PathBuf], filter: &str) -> duckmon::Result<Vec<RecordBatch>> {
    let merged_sql = format!(
        "SELECT file_name FROM parquet_kv_metadata({}) WHERE key = {}::BLOB",
        file_list_literal(files),
        sql_string_literal(COMPACTED_FROM_KEY)
    );
    let mut merged_files = HashSet::new();
    for batch in store.query(&merged_sql)? {
        let names = batch.column(0).as_string::<i32>();
        for row in 0..batch.num_rows() {
            merged_files.insert(PathBuf::from(names.value(row)));
        }
    }
    let files: Vec<PathBuf> = files.iter().filter(|path| !merged_files.contains(*path)).cloned().collect();
    if files.is_empty() {
        return Ok(Vec::new());
    }
    store.query(&format!(
        "SELECT * FROM {} WHERE (\n{filter}\n) ORDER BY ts",
        read_events_sql(&files)
    ))
}

/// One readable line for one event: time, name, message, the set span and
/// trace fields, then the attributes. The resource is left out.
pub fn event_line(batch: &RecordBatch, row: usize) -> String {
    let mut line = String::new();
    if let Some(ts) = batch.column_by_name("ts").and_then(|column| column.as_primitive_opt::<TimestampNanosecondType>())
        && ts.is_valid(row)
    {
        line.push_str(&DateTime::from_timestamp_nanos(ts.value(row)).format(TIMESTAMP_FORMAT).to_string());
    }
    if let Some(name) = text_value(batch, "name", row) {
        line.push(' ');
        line.push_str(name);
    }
    if let Some(message) = text_value(batch, "message", row) {
        line.push(' ');
        line.push_str(&message.replace('\n', "\\n").replace('\r', "\\r"));
    }
    if let Some(duration) = batch.column_by_name("duration_ns").and_then(|column| column.as_primitive_opt::<Int64Type>())
        && duration.is_valid(row)
    {
        line.push_str(&format!(" duration_ns={}", duration.value(row)));
    }
    for column in ["trace_id", "span_id", "parent_id", "attrs"] {
        if let Some(value) = text_value(batch, column, row) {
            line.push_str(&format!(" {column}={value}"));
        }
    }
    line
}

/// The text in a column at `row`, or None when the column is missing or the value is NULL.
fn text_value<'a>(batch: &'a RecordBatch, column: &str, row: usize) -> Option<&'a str> {
    let values = batch.column_by_name(column)?.as_string_opt::<i32>()?;
    values.is_valid(row).then(|| values.value(row))
}

fn parse_interval(text: &str) -> Result<Duration, String> {
    let interval = parse_duration(text)?;
    if interval < Duration::from_millis(100) {
        return Err("the interval must be at least 100ms".to_string());
    }
    Ok(interval)
}
