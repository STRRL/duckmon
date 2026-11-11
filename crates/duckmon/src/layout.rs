use std::io;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

/// File in the store directory that `compact` locks so only one run works at a time.
pub const LOCK_FILE_NAME: &str = ".compact.lock";

/// Name of the directory that holds one UTC date, such as `date=2026-01-31`.
pub fn date_dir_name(day: &str) -> String {
    format!("date={day}")
}

/// Final name of a Parquet file, such as `<writer ID>-00000001.parquet`.
pub fn file_name(writer_id: &str, sequence: u64) -> String {
    format!("{writer_id}-{sequence:08}.parquet")
}

/// Name used while a Parquet file is being written. Readers ignore it.
pub fn temp_file_name(writer_id: &str, sequence: u64) -> String {
    format!(".{}.tmp", file_name(writer_id, sequence))
}

/// Hidden name a day directory gets while retention deletes it.
pub fn deleting_dir_name(day: &str, run_id: &str) -> String {
    format!(".deleting-{}-{run_id}", date_dir_name(day))
}

/// The date of a directory name such as `date=2026-01-31`.
/// Returns None unless the name is exactly what `date_dir_name` writes.
pub fn parse_day_dir_name(name: &str) -> Option<NaiveDate> {
    let day = name.strip_prefix("date=")?;
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    if day_text(date) != day {
        return None;
    }
    Some(date)
}

/// `YYYY-MM-DD` for a date.
pub fn day_text(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// Whether a file name in a day directory is a finished data file.
fn is_data_file_name(name: &str) -> bool {
    name.ends_with(".parquet") && !name.starts_with('.')
}

/// Every `date=*` directory in `dir`. A store directory that does not exist has none.
pub fn day_dirs(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in if_present(std::fs::read_dir(dir))?.into_iter().flatten() {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("date=") && entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

/// Finished data files in one day directory, sorted. A directory that was
/// removed while listing, for example by retention, counts as empty.
pub fn day_data_files(day_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in if_present(std::fs::read_dir(day_dir))?.into_iter().flatten() {
        let entry = entry?;
        if is_data_file_name(&entry.file_name().to_string_lossy()) {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

/// Every finished data file in every `date=*` directory of `dir`, sorted.
pub fn data_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for day_dir in day_dirs(dir)? {
        files.extend(day_data_files(&day_dir)?);
    }
    Ok(files)
}

/// Whether any `date=*` directory holds a finished Parquet file.
pub fn has_parquet_files(dir: &Path) -> io::Result<bool> {
    for day_dir in day_dirs(dir)? {
        if !day_data_files(&day_dir)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Turns the result of a file operation on a path that does not exist into None.
/// Other processes may remove files and day directories at any time.
pub fn if_present<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}
