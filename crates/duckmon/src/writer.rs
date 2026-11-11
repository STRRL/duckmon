use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use duckdb::Connection;
use duckdb::arrow::record_batch::RecordBatch;

use crate::layout::{date_dir_name, file_name, temp_file_name};
use crate::sql::{copy_sql, delete_day_sql};
use crate::store::QueryState;
use crate::{Error, Result};

/// Messages from the store to its writer thread.
pub enum Command {
    /// Add these rows to the buffer table.
    Append(RecordBatch),
    /// Reply once every command sent before this one is done.
    WaitForEarlierCommands(SyncSender<()>),
    /// Write the buffer to Parquet files and reply with the result.
    Flush(SyncSender<Result<()>>),
    /// Same as Flush, then stop the thread.
    Close(SyncSender<Result<()>>),
}

/// State owned by the writer thread.
pub struct Writer {
    pub conn: Connection,
    pub dir: PathBuf,
    pub writer_id: String,
    pub sequence: u64,
    /// Held over rename and delete so a query in this process sees each row once.
    pub query_state: Arc<Mutex<QueryState>>,
}

impl Writer {
    /// Handles commands until Close arrives or the store is gone,
    /// and flushes the buffer every `flush_interval`.
    pub fn run(mut self, commands: Receiver<Command>, flush_interval: Duration) {
        let mut next_flush = Instant::now() + flush_interval;
        let mut saved_error: Option<Error> = None;
        loop {
            let wait = next_flush.saturating_duration_since(Instant::now());
            match commands.recv_timeout(wait) {
                Ok(Command::Append(batch)) => {
                    if let Err(err) = self.append(batch) {
                        saved_error.get_or_insert(err);
                    }
                }
                Ok(Command::WaitForEarlierCommands(reply)) => {
                    let _ = reply.send(());
                }
                Ok(Command::Flush(reply)) => {
                    let result = self.flush();
                    let _ = reply.send(saved_error.take().map_or(result, Err));
                    next_flush = Instant::now() + flush_interval;
                }
                Ok(Command::Close(reply)) => {
                    let result = self.flush();
                    let _ = reply.send(saved_error.take().map_or(result, Err));
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.flush();
                    break;
                }
            }
            // The timed flush is driven by the deadline. While commands keep
            // arriving, recv_timeout never times out, so the timeout alone is not enough.
            if Instant::now() >= next_flush {
                if let Err(err) = self.flush() {
                    saved_error.get_or_insert(err);
                }
                next_flush = Instant::now() + flush_interval;
            }
        }
    }

    fn append(&self, batch: RecordBatch) -> Result<()> {
        let mut appender = self.conn.appender("unflushed_events")?;
        appender.append_record_batch(batch)?;
        appender.flush()?;
        Ok(())
    }

    /// Writes every buffered day to its own new Parquet file.
    /// Stops at the first failure and keeps that day and later days in the buffer.
    fn flush(&mut self) -> Result<()> {
        let days: Vec<String> = self
            .conn
            .prepare("SELECT DISTINCT strftime(date, '%Y-%m-%d') FROM unflushed_events ORDER BY 1")?
            .query_map([], |row| row.get(0))?
            .collect::<duckdb::Result<_>>()?;
        for day in days {
            self.flush_day(&day)?;
        }
        Ok(())
    }

    fn flush_day(&mut self, day: &str) -> Result<()> {
        self.sequence += 1;
        let day_dir = self.dir.join(date_dir_name(day));
        std::fs::create_dir_all(&day_dir)?;
        let final_path = day_dir.join(file_name(&self.writer_id, self.sequence));
        let temp_path = day_dir.join(temp_file_name(&self.writer_id, self.sequence));

        if let Err(err) = self.write_temp_file(day, &temp_path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(err);
        }

        // Holding the query lock over rename and delete means a query in this
        // process sees each row exactly once.
        let _query_lock = self.query_state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(err) = std::fs::rename(&temp_path, &final_path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(err.into());
        }
        self.conn.execute_batch(&delete_day_sql(day))?;
        Ok(())
    }

    fn write_temp_file(&self, day: &str, temp_path: &Path) -> Result<()> {
        self.conn.execute_batch(&copy_sql(day, temp_path))?;
        File::open(temp_path)?.sync_all()?;
        Ok(())
    }
}
