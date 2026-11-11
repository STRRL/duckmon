use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use duckdb::arrow::record_batch::RecordBatch;
use duckdb::{Config, Connection};

use crate::event::events_to_record_batch;
use crate::layout::has_parquet_files;
use crate::sql::{CREATE_BUFFER_TABLE, events_view_sql};
use crate::writer::{Command, Writer};
use crate::{Error, Event, Options, Result};

/// The query connection and whether its `events` view already reads Parquet files.
pub struct QueryState {
    pub conn: Connection,
    pub has_files: bool,
}

/// A telemetry store bound to one directory. Safe to share between threads.
pub struct Store {
    dir: PathBuf,
    resource: serde_json::Map<String, serde_json::Value>,
    commands: SyncSender<Command>,
    query_state: Arc<Mutex<QueryState>>,
    writer_thread: Option<JoinHandle<()>>,
}

/// Opens a store that writes to and reads from `dir`. Creates `dir` when missing.
/// Outside Windows, a path that contains a backslash returns `InvalidDirectory`.
pub fn open(dir: impl AsRef<Path>, options: Options) -> Result<Store> {
    let dir = dir.as_ref().to_path_buf();
    // DuckDB cannot find files below a directory whose name contains a
    // backslash, so the data would be written but never read back.
    if !cfg!(windows) && dir.to_string_lossy().contains('\\') {
        return Err(Error::InvalidDirectory(format!(
            "{} contains a backslash",
            dir.display()
        )));
    }
    std::fs::create_dir_all(&dir)?;

    let writer_conn =
        Connection::open_in_memory_with_flags(Config::default().with("autoinstall_known_extensions", "false")?)?;
    writer_conn.execute_batch("LOAD parquet; LOAD json;")?;
    writer_conn.execute_batch(CREATE_BUFFER_TABLE)?;

    let query_conn = writer_conn.try_clone()?;
    let has_files = has_parquet_files(&dir)?;
    query_conn.execute_batch(&events_view_sql(&dir, has_files))?;
    let query_state = Arc::new(Mutex::new(QueryState {
        conn: query_conn,
        has_files,
    }));

    let writer = Writer {
        conn: writer_conn,
        dir: dir.clone(),
        writer_id: uuid::Uuid::now_v7().simple().to_string(),
        sequence: 0,
        query_state: Arc::clone(&query_state),
    };
    let (commands, receiver) = sync_channel::<Command>(options.channel_capacity);
    let flush_interval = options.flush_interval;
    let writer_thread = std::thread::Builder::new()
        .name("duckmon-writer".to_string())
        .spawn(move || writer.run(receiver, flush_interval))?;

    Ok(Store {
        dir,
        resource: options.resource,
        commands,
        query_state,
        writer_thread: Some(writer_thread),
    })
}

impl Store {
    /// Adds events to the buffer. They reach a Parquet file at the next flush.
    /// Blocks when the writer thread has too many batches waiting.
    pub fn write_events(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let batch = events_to_record_batch(events, &self.resource)?;
        self.commands
            .send(Command::Append(batch))
            .map_err(|_| Error::Closed)
    }

    /// Waits until every event written before this call is in a finished Parquet file.
    /// Also returns any error the writer thread saved since the last flush.
    pub fn flush(&self) -> Result<()> {
        let (reply, answer) = sync_channel(1);
        self.commands
            .send(Command::Flush(reply))
            .map_err(|_| Error::Closed)?;
        answer.recv().map_err(|_| Error::Closed)?
    }

    /// Runs one SQL statement. The relation `events` covers the Parquet
    /// directory plus this process's unflushed buffer.
    pub fn query(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        let (reply, answer) = sync_channel(1);
        self.commands
            .send(Command::WaitForEarlierCommands(reply))
            .map_err(|_| Error::Closed)?;
        answer.recv().map_err(|_| Error::Closed)?;

        let mut query_state = self.query_state.lock().unwrap_or_else(PoisonError::into_inner);
        if !query_state.has_files && has_parquet_files(&self.dir)? {
            query_state.conn.execute_batch(&events_view_sql(&self.dir, true))?;
            query_state.has_files = true;
        }
        let mut statement = query_state.conn.prepare(sql)?;
        let batches = statement.stream_arrow([])?.collect::<duckdb::Result<Vec<_>>>()?;
        Ok(batches)
    }

    /// Writes the buffer one last time and stops the writer thread.
    pub fn close(mut self) -> Result<()> {
        self.shut_down()
    }

    fn shut_down(&mut self) -> Result<()> {
        let Some(writer_thread) = self.writer_thread.take() else {
            return Ok(());
        };
        let (reply, answer) = sync_channel(1);
        let result = match self.commands.send(Command::Close(reply)) {
            Ok(()) => answer.recv().unwrap_or(Err(Error::Closed)),
            Err(_) => Err(Error::Closed),
        };
        let _ = writer_thread.join();
        result
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.shut_down();
    }
}
