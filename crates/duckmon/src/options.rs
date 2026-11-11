use std::time::Duration;

/// Settings for [`crate::open`].
#[derive(Debug, Clone)]
pub struct Options {
    /// How often the buffer is written to a new Parquet file. Default 5 s.
    pub flush_interval: Duration,
    /// How many write_events batches may wait for the writer thread. Default 1024.
    pub channel_capacity: usize,
    /// Stored in the resource column of every event this store writes. Empty is stored as NULL.
    pub resource: serde_json::Map<String, serde_json::Value>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            flush_interval: Duration::from_secs(5),
            channel_capacity: 1024,
            resource: serde_json::Map::new(),
        }
    }
}
