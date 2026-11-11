use arrow::error::ArrowError;
use arrow::json::writer::LineDelimited;
use arrow::record_batch::RecordBatch;
use arrow::util::display::FormatOptions;
use arrow::util::pretty::pretty_format_batches_with_options;

/// How `query` prints its result.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// A text table with a header.
    Table,
    /// One JSON object per row.
    Json,
    /// Comma separated values with a header.
    Csv,
}

/// Every timestamp is printed in UTC with 9 fraction digits.
pub const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.9fZ";

const DATE_FORMAT: &str = "%Y-%m-%d";

/// Formats query results. Table and CSV print the header even when there are no rows.
pub fn format_batches(batches: &[RecordBatch], format: Format) -> Result<Vec<u8>, ArrowError> {
    match format {
        Format::Table => format_table(batches),
        Format::Json => format_json(batches),
        Format::Csv => format_csv(batches),
    }
}

fn format_table(batches: &[RecordBatch]) -> Result<Vec<u8>, ArrowError> {
    let options = FormatOptions::default()
        .with_null("NULL")
        .with_timestamp_format(Some(TIMESTAMP_FORMAT));
    let table = pretty_format_batches_with_options(batches, &options)?;
    Ok(format!("{table}\n").into_bytes())
}

/// JSON Lines. Text columns, `attrs` and `resource` included, stay JSON strings.
pub fn format_json(batches: &[RecordBatch]) -> Result<Vec<u8>, ArrowError> {
    let mut buffer = Vec::new();
    let mut writer = arrow::json::WriterBuilder::new()
        .with_explicit_nulls(true)
        .with_timestamp_format(TIMESTAMP_FORMAT.to_string())
        .with_date_format(DATE_FORMAT.to_string())
        .build::<_, LineDelimited>(&mut buffer);
    for batch in batches {
        writer.write(batch)?;
    }
    writer.finish()?;
    drop(writer);
    Ok(buffer)
}

fn format_csv(batches: &[RecordBatch]) -> Result<Vec<u8>, ArrowError> {
    if let Some(batch) = batches.first() {
        for field in batch.schema().fields() {
            if field.data_type().is_nested() {
                return Err(ArrowError::CsvError(format!(
                    "column {} has type {}, which CSV cannot hold; select to_json({}) instead",
                    field.name(),
                    field.data_type(),
                    field.name()
                )));
            }
        }
    }
    let mut buffer = Vec::new();
    let mut writer = arrow::csv::WriterBuilder::new()
        .with_header(true)
        .with_timestamp_format(TIMESTAMP_FORMAT.to_string())
        .with_date_format(DATE_FORMAT.to_string())
        .build(&mut buffer);
    for batch in batches {
        writer.write(batch)?;
    }
    drop(writer);
    Ok(buffer)
}
