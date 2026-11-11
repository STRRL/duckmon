use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::NaiveDate;
use duckdb::Connection;

use crate::event::{NANOS_PER_DAY, date_from_nanos, nanos_since_epoch};
use crate::layout::{
    LOCK_FILE_NAME, day_data_files, day_dirs, day_text, deleting_dir_name, file_name,
    has_parquet_files, if_present, parse_day_dir_name, temp_file_name,
};
use crate::sql::{
    COMPACTED_FROM_KEY, compact_copy_sql, data_glob, file_list_literal, glob_escape, is_contract_column,
    read_files_sql, sql_string_literal,
};
use crate::store::{check_directory, open_connection};
use crate::{Error, Result};

/// A temporary file older than this belongs to a writer that died.
const STALE_TEMP_AGE: Duration = Duration::from_secs(60 * 60);

/// A merge group is closed once its inputs reach this many bytes.
const TARGET_INPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Most files in one merge group. Bounds the size of the compacted from list.
const MAX_GROUP_FILES: usize = 5000;

/// Settings for [`compact`].
#[derive(Debug, Clone)]
pub struct CompactOptions {
    /// Keep the last N UTC days including today and delete older days.
    /// None deletes nothing. Some(0) deletes every day.
    pub keep_days: Option<u32>,
    /// A day is merged only once it ended at least this long ago, and a file is
    /// merged only once it is at least this old. Default 1 hour.
    pub settle_time: Duration,
    /// Plan and report the actions without changing anything. Takes no lock.
    pub dry_run: bool,
}

impl Default for CompactOptions {
    fn default() -> Self {
        CompactOptions {
            keep_days: None,
            settle_time: Duration::from_secs(60 * 60),
            dry_run: false,
        }
    }
}

/// What one [`compact`] run did, or would do in a dry run, in order.
#[derive(Debug, Default)]
pub struct CompactReport {
    pub actions: Vec<CompactAction>,
}

/// One step of a [`compact`] run.
#[derive(Debug)]
pub enum CompactAction {
    /// A file whose rows are already in a merged file was removed.
    RemoveMergedInput { path: PathBuf, merged_into: PathBuf },
    /// A day directory left by an earlier retention run was removed.
    RemoveDeletedDay { path: PathBuf },
    /// A whole day, `YYYY-MM-DD`, was deleted by retention.
    DeleteDay { day: String, files: usize, bytes: u64 },
    /// A temporary file left by a writer that died was removed.
    RemoveTempFile { path: PathBuf },
    /// Files of one day were merged into one new file.
    MergeFiles {
        day: String,
        inputs: usize,
        input_bytes: u64,
        rows: u64,
        output: PathBuf,
    },
    /// Files of one day were left alone.
    SkipDay { day: String, reason: String },
}

/// Merges the small files of days that have ended into larger ones, and
/// deletes old days when `keep_days` is set. `dir` must exist.
/// Only one run works on a directory at a time; a second run waits.
/// See `docs/contract.md` for what readers see during a run.
pub fn compact(dir: impl AsRef<Path>, options: &CompactOptions) -> Result<CompactReport> {
    let dir = dir.as_ref();
    check_directory(dir)?;
    if !dir.is_dir() {
        return Err(Error::InvalidDirectory(format!("{} does not exist", dir.display())));
    }
    let _lock = if options.dry_run { None } else { Some(lock(dir)?) };

    let run_id = uuid::Uuid::now_v7().simple().to_string();
    // An in memory DuckDB spills into `.tmp` below the current directory unless told otherwise.
    let spill_dir = std::env::temp_dir().join(format!("duckmon-compact-{run_id}"));
    let conn = open_connection()?;
    conn.execute_batch(&format!(
        "SET temp_directory = {}",
        sql_string_literal(&spill_dir.to_string_lossy())
    ))?;

    let now = SystemTime::now();
    let today = date_from_nanos(nanos_since_epoch(now).unwrap_or(0));
    let mut run = CompactRun {
        dir,
        options,
        run_id,
        conn,
        now,
        // Days are compared as i64 so that no `keep_days` value can wrap around.
        last_deleted_day: options.keep_days.map(|days| i64::from(today) - i64::from(days)),
        sequence: 0,
        removed: HashSet::new(),
        actions: Vec::new(),
    };
    let result = run.run_steps();
    let actions = std::mem::take(&mut run.actions);
    drop(run);
    let _ = std::fs::remove_dir_all(&spill_dir);
    result.map(|()| CompactReport { actions })
}

/// Blocks until this process holds the compaction lock of `dir`.
/// The lock is released when the returned file is dropped or the process ends.
fn lock(dir: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE_NAME))?;
    file.lock()?;
    Ok(file)
}

/// State of one compaction run.
struct CompactRun<'a> {
    dir: &'a Path,
    options: &'a CompactOptions,
    run_id: String,
    conn: Connection,
    now: SystemTime,
    /// Days since 1970 up to and including this one are deleted by retention.
    last_deleted_day: Option<i64>,
    sequence: u64,
    /// Files this run removed, or would remove in a dry run.
    removed: HashSet<PathBuf>,
    actions: Vec<CompactAction>,
}

impl CompactRun<'_> {
    fn run_steps(&mut self) -> Result<()> {
        self.remove_merged_inputs()?;
        self.remove_deleted_days()?;
        self.delete_old_days()?;
        self.remove_stale_temp_files()?;
        self.merge_days()
    }

    /// Removes files whose rows a merged file already holds. They are left
    /// behind when a run stops between placing a merged file and removing its inputs.
    fn remove_merged_inputs(&mut self) -> Result<()> {
        if !has_parquet_files(self.dir)? {
            return Ok(());
        }
        let sql = format!(
            "SELECT file_name, decode(value) FROM parquet_kv_metadata({}) WHERE key = {}::BLOB ORDER BY file_name",
            sql_string_literal(&data_glob(self.dir)),
            sql_string_literal(COMPACTED_FROM_KEY)
        );
        let merged_files: Vec<(String, String)> = self
            .conn
            .prepare(&sql)?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<duckdb::Result<_>>()?;
        for (merged_file, compacted_from) in merged_files {
            let merged_into = PathBuf::from(merged_file);
            let Some(day_dir) = merged_into.parent() else {
                continue;
            };
            for name in compacted_from.split(',') {
                // Only plain names in the same directory are accepted, so bad
                // metadata can never point at a file somewhere else.
                if Path::new(name).file_name() != Some(OsStr::new(name)) {
                    continue;
                }
                let path = day_dir.join(name);
                if path == merged_into || !path.exists() {
                    continue;
                }
                if !self.options.dry_run {
                    if_present(std::fs::remove_file(&path))?;
                }
                self.removed.insert(path.clone());
                self.actions.push(CompactAction::RemoveMergedInput {
                    path,
                    merged_into: merged_into.clone(),
                });
            }
        }
        Ok(())
    }

    /// Removes `.deleting-*` directories left by a retention run that stopped early.
    fn remove_deleted_days(&mut self) -> Result<()> {
        for entry in if_present(std::fs::read_dir(self.dir))?.into_iter().flatten() {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().starts_with(".deleting-") || !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            if !self.options.dry_run {
                std::fs::remove_dir_all(&path)?;
            }
            self.actions.push(CompactAction::RemoveDeletedDay { path });
        }
        Ok(())
    }

    /// Deletes every day up to `last_deleted_day`. A rename first takes the
    /// whole day out of every reader's view at once.
    fn delete_old_days(&mut self) -> Result<()> {
        let Some(last_deleted_day) = self.last_deleted_day else {
            return Ok(());
        };
        for day_dir in day_dirs(self.dir)? {
            let Some(date) = dir_date(&day_dir) else {
                continue;
            };
            if epoch_day(date) > last_deleted_day {
                continue;
            }
            let files = day_data_files(&day_dir)?;
            let mut bytes = 0;
            for file in &files {
                bytes += if_present(std::fs::metadata(file))?.map_or(0, |metadata| metadata.len());
            }
            let day = day_text(date);
            if !self.options.dry_run {
                let hidden_dir = self.dir.join(deleting_dir_name(&day, &self.run_id));
                std::fs::rename(&day_dir, &hidden_dir)?;
                std::fs::remove_dir_all(&hidden_dir)?;
            }
            self.actions.push(CompactAction::DeleteDay {
                day,
                files: files.len(),
                bytes,
            });
        }
        Ok(())
    }

    /// Removes `.*.parquet.tmp` files older than `STALE_TEMP_AGE`. A fixed age
    /// is used so that a short settle time never removes a file a live writer is writing.
    fn remove_stale_temp_files(&mut self) -> Result<()> {
        for day_dir in self.kept_day_dirs()? {
            for entry in if_present(std::fs::read_dir(&day_dir))?.into_iter().flatten() {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') || !name.ends_with(".parquet.tmp") {
                    continue;
                }
                let path = entry.path();
                if !self.is_older_than(&path, STALE_TEMP_AGE)? {
                    continue;
                }
                if !self.options.dry_run {
                    if_present(std::fs::remove_file(&path))?;
                }
                self.actions.push(CompactAction::RemoveTempFile { path });
            }
        }
        Ok(())
    }

    /// Merges the settled files of every day that ended at least the settle time ago.
    fn merge_days(&mut self) -> Result<()> {
        let settle_nanos = i64::try_from(self.options.settle_time.as_nanos()).unwrap_or(i64::MAX);
        let now_nanos = nanos_since_epoch(self.now).unwrap_or(0);
        for day_dir in self.kept_day_dirs()? {
            let Some(date) = dir_date(&day_dir) else {
                continue;
            };
            let day_end = (epoch_day(date) + 1).saturating_mul(NANOS_PER_DAY);
            if day_end.saturating_add(settle_nanos) > now_nanos {
                continue;
            }
            let mut inputs = Vec::new();
            for path in day_data_files(&day_dir)? {
                if self.removed.contains(&path) || !self.is_older_than(&path, self.options.settle_time)? {
                    continue;
                }
                if let Some(metadata) = if_present(std::fs::metadata(&path))? {
                    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    inputs.push((path, name, metadata.len()));
                }
            }
            let names_and_sizes: Vec<(String, u64)> =
                inputs.iter().map(|(_, name, size)| (name.clone(), *size)).collect();
            for group in plan_groups(&names_and_sizes, TARGET_INPUT_BYTES) {
                let group: Vec<&(PathBuf, String, u64)> = group.iter().map(|&index| &inputs[index]).collect();
                self.merge_group(&day_dir, &day_text(date), &group)?;
            }
        }
        Ok(())
    }

    /// Writes one merged file for `group`, puts it in place, then removes the inputs.
    fn merge_group(&mut self, day_dir: &Path, day: &str, group: &[&(PathBuf, String, u64)]) -> Result<()> {
        let paths: Vec<PathBuf> = group.iter().map(|(path, _, _)| path.clone()).collect();
        let list = file_list_literal(&paths);

        // Files from a newer duckmon may hold columns this version does not
        // know. Merging them would drop those columns, so the group is left alone.
        let contract_files: i64 = self.conn.query_row(
            &format!(
                "SELECT count(DISTINCT file_name) FROM parquet_kv_metadata({list}) \
                 WHERE key = 'duckmon_contract'::BLOB AND value = '1'::BLOB"
            ),
            [],
            |row| row.get(0),
        )?;
        if contract_files != paths.len() as i64 {
            self.actions.push(CompactAction::SkipDay {
                day: day.to_string(),
                reason: "some files do not carry duckmon_contract = '1'".to_string(),
            });
            return Ok(());
        }
        let columns: Vec<String> = self
            .conn
            .prepare(&format!("DESCRIBE SELECT * FROM {}", read_files_sql(&paths)))?
            .query_map([], |row| row.get(0))?
            .collect::<duckdb::Result<_>>()?;
        if let Some(column) = columns.iter().find(|column| !is_contract_column(column)) {
            self.actions.push(CompactAction::SkipDay {
                day: day.to_string(),
                reason: format!("column {column} is not part of contract version 1"),
            });
            return Ok(());
        }
        let expected_rows: i64 = self.conn.query_row(
            &format!("SELECT sum(num_rows)::BIGINT FROM parquet_file_metadata({list})"),
            [],
            |row| row.get(0),
        )?;

        self.sequence += 1;
        let output = day_dir.join(file_name(&self.run_id, self.sequence));
        let action = CompactAction::MergeFiles {
            day: day.to_string(),
            inputs: group.len(),
            input_bytes: group.iter().map(|(_, _, size)| size).sum(),
            rows: expected_rows as u64,
            output: output.clone(),
        };
        if self.options.dry_run {
            self.actions.push(action);
            return Ok(());
        }

        let temp_path = day_dir.join(temp_file_name(&self.run_id, self.sequence));
        let names: Vec<&str> = group.iter().map(|(_, name, _)| name.as_str()).collect();
        if let Err(err) = self.write_merged_file(&paths, &temp_path, &names.join(","), expected_rows) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(err);
        }
        if let Err(err) = std::fs::rename(&temp_path, &output) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(err.into());
        }
        // The rename must be on disk before any input is gone, or a crash could lose rows.
        #[cfg(unix)]
        File::open(day_dir)?.sync_all()?;
        for path in &paths {
            if_present(std::fs::remove_file(path))?;
        }
        self.actions.push(action);
        Ok(())
    }

    fn write_merged_file(&self, inputs: &[PathBuf], temp_path: &Path, compacted_from: &str, expected_rows: i64) -> Result<()> {
        self.conn
            .execute_batch(&compact_copy_sql(inputs, temp_path, compacted_from))?;
        File::open(temp_path)?.sync_all()?;
        let rows: i64 = self.conn.query_row(
            &format!(
                "SELECT num_rows FROM parquet_file_metadata({})",
                sql_string_literal(&glob_escape(&temp_path.to_string_lossy()))
            ),
            [],
            |row| row.get(0),
        )?;
        if rows != expected_rows {
            return Err(Error::Compact(format!(
                "{} holds {rows} rows, its inputs hold {expected_rows}",
                temp_path.display()
            )));
        }
        Ok(())
    }

    /// Day directories that retention keeps.
    fn kept_day_dirs(&self) -> Result<Vec<PathBuf>> {
        let mut kept = day_dirs(self.dir)?;
        if let Some(last_deleted_day) = self.last_deleted_day {
            kept.retain(|day_dir| dir_date(day_dir).is_none_or(|date| epoch_day(date) > last_deleted_day));
        }
        Ok(kept)
    }

    /// Whether the file was last changed at least `age` ago. A missing file is not.
    fn is_older_than(&self, path: &Path, age: Duration) -> Result<bool> {
        let Some(metadata) = if_present(std::fs::metadata(path))? else {
            return Ok(false);
        };
        let modified = metadata.modified()?;
        Ok(self.now.duration_since(modified).unwrap_or(Duration::ZERO) >= age)
    }
}

/// Splits files into merge groups and returns the indices in each group.
/// Files are taken in name order and a group is closed once it reaches
/// `target_bytes` or `MAX_GROUP_FILES`. A group of one file is dropped,
/// which leaves a day that is already compact alone.
fn plan_groups(files: &[(String, u64)], target_bytes: u64) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|&a, &b| files[a].0.cmp(&files[b].0));
    let mut groups = Vec::new();
    let mut group = Vec::new();
    let mut group_bytes = 0;
    for index in order {
        group.push(index);
        group_bytes += files[index].1;
        if group_bytes >= target_bytes || group.len() >= MAX_GROUP_FILES {
            groups.push(std::mem::take(&mut group));
            group_bytes = 0;
        }
    }
    groups.push(group);
    groups.retain(|group| group.len() > 1);
    groups
}

/// The date of a `date=` directory, or None for any other name.
fn dir_date(day_dir: &Path) -> Option<NaiveDate> {
    parse_day_dir_name(&day_dir.file_name()?.to_string_lossy())
}

/// Days since 1970 of a date.
fn epoch_day(date: NaiveDate) -> i64 {
    i64::from(date.to_epoch_days())
}
