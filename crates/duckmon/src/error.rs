use std::fmt;

/// Every error duckmon can return.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// DuckDB reported an error.
    Database(duckdb::Error),
    /// A file or directory operation failed.
    File(std::io::Error),
    /// An event could not be stored, for example because its time is before 1970.
    InvalidEvent(String),
    /// The store directory path cannot be used.
    InvalidDirectory(String),
    /// The store is closed or its writer thread has stopped.
    Closed,
    /// A merged file did not hold the same rows as its inputs.
    Compact(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Database(err) => write!(f, "database error: {err}"),
            Error::File(err) => write!(f, "file error: {err}"),
            Error::InvalidEvent(reason) => write!(f, "invalid event: {reason}"),
            Error::InvalidDirectory(reason) => write!(f, "invalid directory: {reason}"),
            Error::Closed => write!(f, "store is closed"),
            Error::Compact(reason) => write!(f, "compaction failed: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Database(err) => Some(err),
            Error::File(err) => Some(err),
            Error::InvalidEvent(_) | Error::InvalidDirectory(_) | Error::Closed | Error::Compact(_) => None,
        }
    }
}

impl From<duckdb::Error> for Error {
    fn from(err: duckdb::Error) -> Self {
        Error::Database(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::File(err)
    }
}
