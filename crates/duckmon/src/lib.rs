//! duckmon is an embedded telemetry store.
//!
//! An application writes events through a [`Store`]. Events land as Parquet
//! files in a hive partitioned directory, and queries are SQL run by an in
//! process DuckDB. Many processes may share one directory: each one only adds
//! its own files, and [`compact`] merges small files of past days. See `docs/contract.md` for the on disk format.

mod compact;
mod error;
mod event;
mod layout;
mod options;
mod sql;
mod store;
mod writer;

pub use duckdb::arrow;
pub use compact::{CompactAction, CompactOptions, CompactReport, compact};
pub use error::Error;
pub use event::Event;
pub use layout::data_files;
pub use options::Options;
pub use sql::{COMPACTED_FROM_KEY, file_list_literal, read_events_sql, sql_string_literal};
pub use store::{Store, open};

/// Result type used by every duckmon function.
pub type Result<T> = std::result::Result<T, Error>;
