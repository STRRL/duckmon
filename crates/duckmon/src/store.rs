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

/// The query connection and what its `events` view reads. Only `Store::query`
/// changes the view; the writer only takes the lock.
pub struct QueryState {
    conn: Connection,
    /// None until the first query creates the view. Then whether the view reads Parquet files.
    view_reads_files: Option<bool>,
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
    check_directory(&dir)?;
    std::fs::create_dir_all(&dir)?;

    let writer_conn = open_connection()?;
    writer_conn.execute_batch(CREATE_BUFFER_TABLE)?;

    // Only `query` creates the `events` view. Binding the view opens Parquet
    // files, and a file that `compact` removes at that moment would make
    // writing fail, so opening and writing never read Parquet.
    let query_state = Arc::new(Mutex::new(QueryState {
        conn: writer_conn.try_clone()?,
        view_reads_files: None,
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

/// Returns `InvalidDirectory` outside Windows when the path contains a backslash.
pub(crate) fn check_directory(dir: &Path) -> Result<()> {
    // DuckDB cannot find files below a directory whose name contains a
    // backslash, so the data would be written but never read back.
    if !cfg!(windows) && dir.to_string_lossy().contains('\\') {
        return Err(Error::InvalidDirectory(format!(
            "{} contains a backslash",
            dir.display()
        )));
    }
    Ok(())
}

/// A new in memory DuckDB with the Parquet and JSON extensions loaded.
pub(crate) fn open_connection() -> Result<Connection> {
    let conn =
        Connection::open_in_memory_with_flags(Config::default().with("autoinstall_known_extensions", "false")?)?;
    conn.execute_batch("LOAD parquet; LOAD json;")?;
    Ok(conn)
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

    /// Runs SQL. The relation `events` covers the Parquet directory plus this
    /// process's unflushed buffer. Rows this process writes are seen exactly once,
    /// in a file or in the buffer.
    /// When `sql` holds several statements, all of them run and the result of the last is returned.
    /// While `compact` replaces a group of files of a past day, a query that reads
    /// `events` can fail with an IO error or count that group's rows twice;
    /// running it again gives the right answer. See `docs/contract.md`.
    /// Always returns at least one batch, so the result schema is known even with zero rows.
    pub fn query(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        let (reply, answer) = sync_channel(1);
        self.commands
            .send(Command::WaitForEarlierCommands(reply))
            .map_err(|_| Error::Closed)?;
        answer.recv().map_err(|_| Error::Closed)?;

        let mut query_state = self.query_state.lock().unwrap_or_else(PoisonError::into_inner);
        // The view is created by the first query, and created again only when
        // the directory changed between holding data files and holding none.
        // Retention can delete every file, and a view that reads files fails
        // when there are none.
        let has_files = has_parquet_files(&self.dir)?;
        if query_state.view_reads_files != Some(has_files) {
            query_state.conn.execute_batch(&events_view_sql(&self.dir, has_files))?;
            query_state.view_reads_files = Some(has_files);
        }
        let mut statement = query_state.conn.prepare(sql)?;
        let stream = statement.stream_arrow([])?;
        let schema = stream.get_schema();
        let mut batches = stream.collect::<duckdb::Result<Vec<_>>>()?;
        if batches.is_empty() {
            batches.push(RecordBatch::new_empty(schema));
        }
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
