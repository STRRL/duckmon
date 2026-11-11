use arrow::ipc::writer::StreamWriter;
use arrow::record_batch::RecordBatch;

use crate::error::CallError;

/// Writes query results as an Arrow IPC stream: the schema, then every batch.
/// `batches` must not be empty, because the schema comes from the first batch.
pub fn arrow_ipc_stream(batches: &[RecordBatch]) -> Result<Vec<u8>, CallError> {
    let first = batches
        .first()
        .ok_or_else(|| CallError::internal("query returned no batch, so the schema is unknown"))?;
    let failed = |err: arrow::error::ArrowError| CallError::internal(format!("could not write Arrow IPC: {err}"));
    let mut buffer = Vec::new();
    let mut writer = StreamWriter::try_new(&mut buffer, &first.schema()).map_err(failed)?;
    for batch in batches {
        writer.write(batch).map_err(failed)?;
    }
    writer.finish().map_err(failed)?;
    drop(writer);
    Ok(buffer)
}
