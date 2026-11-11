use std::path::Path;

/// The in memory buffer table. `date` is filled in by Rust, never by a SQL cast.
pub const CREATE_BUFFER_TABLE: &str = "CREATE TABLE unflushed_events(
    ts TIMESTAMP_NS NOT NULL,
    name VARCHAR NOT NULL,
    message VARCHAR,
    duration_ns BIGINT,
    trace_id VARCHAR,
    span_id VARCHAR,
    parent_id VARCHAR,
    resource VARCHAR,
    attrs VARCHAR,
    date DATE NOT NULL)";

/// Columns that the contract says allow NULL, in file order after `ts` and `name`.
const NULLABLE_COLUMNS: [&str; 7] = [
    "message",
    "duration_ns",
    "trace_id",
    "span_id",
    "parent_id",
    "resource",
    "attrs",
];

/// Puts text inside single quotes for SQL, writing each `'` as `''`.
pub fn sql_string_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Makes DuckDB glob patterns match `text` literally. Each glob character
/// goes inside its own bracket class, so `[` becomes `[[]` and `*` becomes `[*]`.
pub fn glob_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '[' | ']' | '*' | '?' => {
                escaped.push('[');
                escaped.push(character);
                escaped.push(']');
            }
            _ => escaped.push(character),
        }
    }
    escaped
}

/// The `events` view: Parquet files in `dir` when there are any, plus the buffer.
pub fn events_view_sql(dir: &Path, has_files: bool) -> String {
    if !has_files {
        return "CREATE OR REPLACE TEMP VIEW events AS SELECT * FROM unflushed_events".to_string();
    }
    let glob = format!("{}/date=*/*.parquet", glob_escape(&dir.to_string_lossy()));
    format!(
        "CREATE OR REPLACE TEMP VIEW events AS \
         SELECT * FROM read_parquet({}, hive_partitioning = true, union_by_name = true, hive_types = {{'date': 'DATE'}}) \
         UNION ALL BY NAME \
         SELECT * FROM unflushed_events",
        sql_string_literal(&glob)
    )
}

/// Writes the buffered rows of one day, `YYYY-MM-DD`, to a Parquet file.
pub fn copy_sql(day: &str, temp_path: &Path) -> String {
    // DuckDB marks a Parquet column REQUIRED when the statistics of this
    // query show no NULL in it. A CASE with a NULL branch keeps the value
    // and makes the statistics allow NULL, so the column is written OPTIONAL
    // in every file, as the contract says.
    let nullable_columns: Vec<String> = NULLABLE_COLUMNS
        .iter()
        .map(|column| format!("CASE WHEN {column} IS NULL THEN NULL ELSE {column} END AS {column}"))
        .collect();
    format!(
        "COPY (SELECT ts, name, {} \
         FROM unflushed_events WHERE date = DATE {} ORDER BY ts) \
         TO {} (FORMAT parquet, COMPRESSION zstd, KV_METADATA {{duckmon_contract: '1'}})",
        nullable_columns.join(", "),
        sql_string_literal(day),
        sql_string_literal(&temp_path.to_string_lossy())
    )
}

/// Removes the buffered rows of one day after its file is in place.
pub fn delete_day_sql(day: &str) -> String {
    format!("DELETE FROM unflushed_events WHERE date = DATE {}", sql_string_literal(day))
}
