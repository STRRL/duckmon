use std::io::Write;
use std::path::Path;

use crate::Failure;
use crate::output::{Format, format_batches};

/// Runs the SQL and prints the result to stdout. When it holds several
/// statements, all of them run and only the result of the last one is printed.
pub fn run(dir: &Path, sql: &str, format: Format) -> Result<(), Failure> {
    let store = duckmon::open(dir, duckmon::Options::default())?;
    let batches = store.query(sql)?;
    store.close()?;
    let text = format_batches(&batches, format).map_err(|err| Failure::Runtime(err.to_string()))?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&text)?;
    stdout.flush()?;
    Ok(())
}
