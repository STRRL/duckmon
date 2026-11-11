use std::io;
use std::path::Path;

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

/// Whether any `date=*` directory holds a finished Parquet file.
pub fn has_parquet_files(dir: &Path) -> io::Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("date=") || !entry.file_type()?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(entry.path())? {
            let name = file?.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".parquet") && !name.starts_with('.') {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
