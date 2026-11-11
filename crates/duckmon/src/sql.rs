use std::path::{Path, PathBuf};

/// Parquet key value metadata on a merged file: the comma separated names
/// of the files, in the same directory, whose rows it holds.
pub const COMPACTED_FROM_KEY: &str = "duckmon_compacted_from";

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
    format!(
        "CREATE OR REPLACE TEMP VIEW events AS \
         SELECT * FROM {} \
         UNION ALL BY NAME \
         SELECT * FROM unflushed_events",
        read_with_date_sql(&sql_string_literal(&data_glob(dir)))
    )
}

/// The pattern that matches every data file in `dir`, with `dir` escaped.
pub fn data_glob(dir: &Path) -> String {
    format!("{}/date=*/*.parquet", glob_escape(&dir.to_string_lossy()))
}

/// Reads Parquet files the way the contract says, with `date` from the path.
/// `source` is a SQL string literal or list of them.
fn read_with_date_sql(source: &str) -> String {
    format!(
        "read_parquet({source}, hive_partitioning = true, union_by_name = true, hive_types = {{'date': 'DATE'}})"
    )
}

/// Reads the listed data files the way the `events` view does, `date` column included.
pub fn read_events_sql(paths: &[PathBuf]) -> String {
    read_with_date_sql(&file_list_literal(paths))
}

/// Whether a column is one the contract defines for data files.
pub fn is_contract_column(column: &str) -> bool {
    column == "ts" || column == "name" || NULLABLE_COLUMNS.contains(&column)
}

/// The columns of a data file, in file order. Used by every query that writes one.
fn select_list() -> String {
    // DuckDB marks a Parquet column REQUIRED when the statistics of this
    // query show no NULL in it. A CASE with a NULL branch keeps the value
    // and makes the statistics allow NULL, so the column is written OPTIONAL
    // in every file, as the contract says.
    let nullable_columns: Vec<String> = NULLABLE_COLUMNS
        .iter()
        .map(|column| format!("CASE WHEN {column} IS NULL THEN NULL ELSE {column} END AS {column}"))
        .collect();
    format!("ts, name, {}", nullable_columns.join(", "))
}

/// Writes the buffered rows of one day, `YYYY-MM-DD`, to a Parquet file.
pub fn copy_sql(day: &str, temp_path: &Path) -> String {
    format!(
        "COPY (SELECT {} \
         FROM unflushed_events WHERE date = DATE {} ORDER BY ts) \
         TO {} (FORMAT parquet, COMPRESSION zstd, KV_METADATA {{duckmon_contract: '1'}})",
        select_list(),
        sql_string_literal(day),
        sql_string_literal(&temp_path.to_string_lossy())
    )
}

/// A SQL list of Parquet paths, such as `['a.parquet', 'b.parquet']`.
/// Each path is glob escaped, because DuckDB treats every element as a pattern.
pub fn file_list_literal(paths: &[PathBuf]) -> String {
    let elements: Vec<String> = paths
        .iter()
        .map(|path| sql_string_literal(&glob_escape(&path.to_string_lossy())))
        .collect();
    format!("[{}]", elements.join(", "))
}

/// Reads data files as stored, without the `date` column from their path.
pub fn read_files_sql(inputs: &[PathBuf]) -> String {
    format!(
        "read_parquet({}, union_by_name = true, hive_partitioning = false)",
        file_list_literal(inputs)
    )
}

/// Merges data files of one day into one Parquet file, sorted by `ts`.
/// `compacted_from` is the comma separated list of the input file names.
pub fn compact_copy_sql(inputs: &[PathBuf], temp_path: &Path, compacted_from: &str) -> String {
    format!(
        "COPY (SELECT {} FROM {} ORDER BY ts) \
         TO {} (FORMAT parquet, COMPRESSION zstd, KV_METADATA {{duckmon_contract: '1', {}: {}}})",
        select_list(),
        read_files_sql(inputs),
        sql_string_literal(&temp_path.to_string_lossy()),
        COMPACTED_FROM_KEY,
        sql_string_literal(compacted_from)
    )
}

/// Removes the buffered rows of one day after its file is in place.
pub fn delete_day_sql(day: &str) -> String {
    format!("DELETE FROM unflushed_events WHERE date = DATE {}", sql_string_literal(day))
}
