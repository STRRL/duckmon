use std::io::Write;
use std::path::Path;
use std::time::Duration;

use duckmon::{CompactAction, CompactOptions};

use crate::Failure;
use crate::emit::parse_duration;

/// Arguments of `duckmon compact`.
#[derive(Debug, clap::Args)]
pub struct CompactArgs {
    /// Keep the last N UTC days including today and delete older days.
    /// Without this flag nothing is deleted.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub keep_days: Option<u32>,
    /// A day is merged only once it ended at least this long ago, and a file
    /// only once it is this old. A whole number with a unit (ns, us, ms, s, m, h).
    #[arg(long, default_value = "1h", value_parser = parse_duration)]
    pub settle: Duration,
    /// Print what would be done and change nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Compacts the store and prints one line per action.
pub fn run(dir: &Path, args: &CompactArgs) -> Result<(), Failure> {
    let options = CompactOptions {
        keep_days: args.keep_days,
        settle_time: args.settle,
        dry_run: args.dry_run,
    };
    let report = duckmon::compact(dir, &options)?;
    let mut text = String::new();
    for action in &report.actions {
        text.push_str(&action_line(action, dir, args.dry_run));
        text.push('\n');
    }
    if report.actions.is_empty() {
        text.push_str("nothing to do\n");
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

/// One line that describes an action. Paths are shown relative to the store directory.
pub fn action_line(action: &CompactAction, dir: &Path, dry_run: bool) -> String {
    let (remove, delete, merge) = if dry_run {
        ("would remove", "would delete", "would merge")
    } else {
        ("removed", "deleted", "merged")
    };
    let short = |path: &Path| path.strip_prefix(dir).unwrap_or(path).display().to_string();
    match action {
        CompactAction::RemoveMergedInput { path, merged_into } => format!(
            "{remove} {}: already merged into {}",
            short(path),
            merged_into.file_name().unwrap_or_default().to_string_lossy()
        ),
        CompactAction::RemoveDeletedDay { path } => {
            format!("{remove} leftover directory {}", short(path))
        }
        CompactAction::DeleteDay { day, files, bytes } => {
            format!("{delete} day {day}: {files} files, {}", size_text(*bytes))
        }
        CompactAction::RemoveTempFile { path } => {
            format!("{remove} stale temporary file {}", short(path))
        }
        CompactAction::MergeFiles {
            day,
            inputs,
            input_bytes,
            rows,
            output,
        } => format!(
            "{merge} day {day}: {inputs} files, {} into {}, {rows} rows",
            size_text(*input_bytes),
            output.file_name().unwrap_or_default().to_string_lossy()
        ),
        CompactAction::SkipDay { day, reason } => format!("skipped day {day}: {reason}"),
    }
}

/// A byte count with one decimal, such as `14.1 MB`.
fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64 / 1000.0;
    let mut unit = 0;
    while size >= 1000.0 && unit + 1 < UNITS.len() {
        size /= 1000.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}
