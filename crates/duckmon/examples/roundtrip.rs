//! Writes events and reads them back.
//!
//! cargo run -p duckmon --release --example roundtrip -- write <dir> <count> <hold_seconds>
//! cargo run -p duckmon --release --example roundtrip -- count <dir> <expected>

use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use duckmon::arrow::array::{Array, Int64Array};
use duckmon::arrow::compute::cast;
use duckmon::arrow::datatypes::DataType;
use duckmon::{Event, Options, Store};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["write", dir, count, hold_seconds] => write(dir, parse(count), parse(hold_seconds)),
        ["count", dir, expected] => count(dir, parse(expected)),
        _ => {
            eprintln!("usage: roundtrip write <dir> <count> <hold_seconds>");
            eprintln!("       roundtrip count <dir> <expected>");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn parse(text: &str) -> u64 {
    text.parse().unwrap_or_else(|_| panic!("not a number: {text}"))
}

fn write(dir: &str, count: u64, hold_seconds: u64) -> duckmon::Result<bool> {
    let mut resource = serde_json::Map::new();
    resource.insert("service.name".to_string(), "roundtrip".into());
    let store = duckmon::open(
        dir,
        Options {
            flush_interval: Duration::from_secs(1),
            resource,
            ..Options::default()
        },
    )?;

    let run = uuid::Uuid::now_v7().simple().to_string();
    let now = SystemTime::now();
    let mut events = Vec::new();
    let mut expected_span_ns: i64 = 0;
    let mut expected_metric_sum: i64 = 0;
    for i in 0..count {
        let mut event = match i % 3 {
            0 => {
                let mut event = Event::new("app.http");
                event.message = Some(format!("handled request {i}"));
                event.attrs.insert("level".to_string(), "info".into());
                event
            }
            1 => {
                let mut event = Event::new("http.request");
                event.duration = Some(Duration::from_nanos(i * 1000));
                event.trace_id = Some(uuid::Uuid::now_v7().simple().to_string());
                event.span_id = Some(format!("{:016x}", i + 1));
                expected_span_ns += (i * 1000) as i64;
                event
            }
            _ => {
                let mut event = Event::new("process.stats");
                event.attrs.insert("value".to_string(), i.into());
                event.attrs.insert("cpu.percent".to_string(), 0.5.into());
                expected_metric_sum += i as i64;
                event
            }
        };
        event.ts = now - Duration::from_secs(i * 20);
        event.attrs.insert("run".to_string(), run.clone().into());
        events.push(event);
    }

    let (first_half, second_half) = events.split_at(events.len() / 2);
    for batch in first_half.chunks(100) {
        store.write_events(batch)?;
    }
    store.flush()?;
    for batch in second_half.chunks(100) {
        store.write_events(batch)?;
    }

    let sql = format!(
        "SELECT count(*) AS rows,
                sum(duration_ns) FILTER (WHERE span_id IS NOT NULL) AS span_ns,
                sum((attrs->>'value')::BIGINT) FILTER (WHERE name = 'process.stats') AS metric_sum,
                count(*) FILTER (WHERE date <> CAST(strftime(ts, '%Y-%m-%d') AS DATE)) AS wrong_date
         FROM events WHERE attrs->>'run' = '{run}'"
    );
    let row = first_row(&store, &sql)?;
    let expected = [count as i64, expected_span_ns, expected_metric_sum, 0];
    let buffer_rows = first_row(&store, "SELECT count(*) FROM unflushed_events")?[0];

    if row != expected {
        println!("roundtrip mismatch run={run}");
        println!("expected rows, span_ns, metric_sum, wrong_date = {expected:?}");
        println!("actual   rows, span_ns, metric_sum, wrong_date = {row:?}");
        return Ok(false);
    }
    println!(
        "roundtrip ok run={run} rows={} span_ns={} metric_sum={}",
        row[0], row[1], row[2]
    );
    println!("buffer rows={buffer_rows}");

    std::thread::sleep(Duration::from_secs(hold_seconds));
    store.close()?;
    Ok(true)
}

fn count(dir: &str, expected: u64) -> duckmon::Result<bool> {
    let store = duckmon::open(dir, Options::default())?;
    let rows = first_row(&store, "SELECT count(*) FROM events")?[0];
    println!("{rows}");
    store.close()?;
    Ok(rows == expected as i64)
}

/// Reads the first row of a query whose columns are all integers.
/// A NULL value is shown as i64::MIN.
fn first_row(store: &Store, sql: &str) -> duckmon::Result<Vec<i64>> {
    let batches = store.query(sql)?;
    let batch = batches
        .iter()
        .find(|batch| batch.num_rows() > 0)
        .expect("query returned no rows");
    let mut values = Vec::new();
    for column in batch.columns() {
        let column = cast(column, &DataType::Int64).expect("column is not an integer");
        let column = column
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("cast gives Int64Array");
        values.push(if column.is_null(0) { i64::MIN } else { column.value(0) });
    }
    Ok(values)
}
