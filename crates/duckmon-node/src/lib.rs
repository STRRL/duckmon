//! Node-API addon for the npm package `duckmon`. It only turns JavaScript
//! values into C ABI calls and back. Every store operation goes through the
//! `duckmon_*` functions of `duckmon-ffi`.

mod call;

use std::sync::{Arc, Mutex, PoisonError};

use duckmon_ffi::{
    duckmon_abi_version, duckmon_compact, duckmon_open, duckmon_store_close, duckmon_store_flush,
    duckmon_store_query, duckmon_store_write_events,
};
use napi::bindgen_prelude::{AsyncTask, Buffer, Either};
use napi_derive::napi;

use crate::call::{Handle, NativeError, NativeTask, bytes_result, c_text, check};

/// JSON Lines text that is written exactly once. The thread pool task and the
/// exit handler both try to write it; the first one takes the text, and holds
/// the lock until its write is done, so the other one waits and then finds
/// nothing left to write.
#[napi]
pub struct NativeBatch {
    text: Arc<Mutex<Option<String>>>,
}

#[napi]
impl NativeBatch {
    #[napi(constructor)]
    pub fn new(json_lines: String) -> NativeBatch {
        NativeBatch {
            text: Arc::new(Mutex::new(Some(json_lines))),
        }
    }
}

/// An open store. The handle is shared with running tasks, so it is freed
/// only after the last task and this object are gone.
#[napi]
pub struct NativeStore {
    handle: Arc<Handle>,
}

#[napi]
impl NativeStore {
    /// Writes a batch on the thread pool, unless it was written already.
    #[napi]
    pub fn write_events(&self, batch: &NativeBatch) -> AsyncTask<NativeTask<Option<NativeError>>> {
        let handle = Arc::clone(&self.handle);
        let text = Arc::clone(&batch.text);
        NativeTask::start(move || write_batch(&handle, &text))
    }

    /// Writes a batch on the calling thread, unless it was written already. Waits
    /// while a thread pool task writes it. Only for the exit handler.
    #[napi]
    pub fn write_events_sync(&self, batch: &NativeBatch) -> Option<NativeError> {
        write_batch(&self.handle, &batch.text)
    }

    #[napi]
    pub fn flush(&self) -> AsyncTask<NativeTask<Option<NativeError>>> {
        let handle = Arc::clone(&self.handle);
        // SAFETY: the handle came from duckmon_open and is freed only when the last Arc is dropped.
        NativeTask::start(move || check(unsafe { duckmon_store_flush(handle.0) }))
    }

    /// Runs SQL on the thread pool and resolves with an Arrow IPC stream.
    #[napi]
    pub fn query(&self, sql: String) -> AsyncTask<NativeTask<Either<Buffer, NativeError>>> {
        let handle = Arc::clone(&self.handle);
        NativeTask::start(move || {
            let result = c_text(sql, "sql").and_then(|sql| {
                bytes_result(|bytes| {
                    // SAFETY: the handle is alive while the Arc is held, `sql` is a CString
                    // that outlives the call, and `bytes` points to a local `Bytes`.
                    unsafe { duckmon_store_query(handle.0, sql.as_ptr(), bytes) }
                })
            });
            buffer_or_error(result)
        })
    }

    #[napi]
    pub fn close(&self) -> AsyncTask<NativeTask<Option<NativeError>>> {
        let handle = Arc::clone(&self.handle);
        // SAFETY: the handle came from duckmon_open and is freed only when the last Arc is dropped.
        NativeTask::start(move || check(unsafe { duckmon_store_close(handle.0) }))
    }

    /// Closes on the calling thread. Only for the exit handler.
    #[napi]
    pub fn close_sync(&self) -> Option<NativeError> {
        // SAFETY: the handle came from duckmon_open and lives as long as `self`.
        check(unsafe { duckmon_store_close(self.handle.0) })
    }
}

#[napi]
pub fn abi_version() -> u32 {
    duckmon_abi_version()
}

/// Opens a store on the thread pool.
#[napi]
pub fn open(dir: String, options_json: String) -> AsyncTask<NativeTask<Either<NativeStore, NativeError>>> {
    NativeTask::start(move || match open_handle(dir, options_json) {
        Ok(handle) => Either::A(NativeStore {
            handle: Arc::new(handle),
        }),
        Err(err) => Either::B(err),
    })
}

/// Compacts a store directory on the thread pool and resolves with the report as UTF 8 JSON.
#[napi]
pub fn compact(dir: String, options_json: String) -> AsyncTask<NativeTask<Either<Buffer, NativeError>>> {
    NativeTask::start(move || {
        let result = c_text(dir, "dir").and_then(|dir| {
            let options_json = c_text(options_json, "options JSON")?;
            bytes_result(|bytes| {
                // SAFETY: both texts are CStrings that outlive the call, and `bytes` points to a local `Bytes`.
                unsafe { duckmon_compact(dir.as_ptr(), options_json.as_ptr(), bytes) }
            })
        });
        buffer_or_error(result)
    })
}

/// Writes the batch text if nobody took it yet. The lock is held during the
/// write, so a second caller returns only after the first write is done.
fn write_batch(handle: &Handle, text: &Mutex<Option<String>>) -> Option<NativeError> {
    let mut text = text.lock().unwrap_or_else(PoisonError::into_inner);
    let json_lines = text.take()?;
    match c_text(json_lines, "events") {
        // SAFETY: the handle is alive while the caller holds it, and `json_lines` is a CString that outlives the call.
        Ok(json_lines) => check(unsafe { duckmon_store_write_events(handle.0, json_lines.as_ptr()) }),
        Err(err) => Some(err),
    }
}

fn open_handle(dir: String, options_json: String) -> Result<Handle, NativeError> {
    let dir = c_text(dir, "dir")?;
    let options_json = c_text(options_json, "options JSON")?;
    let mut store = std::ptr::null_mut();
    // SAFETY: both texts are CStrings that outlive the call, and `store` is a local pointer to write to.
    if let Some(err) = check(unsafe { duckmon_open(dir.as_ptr(), options_json.as_ptr(), &mut store) }) {
        return Err(err);
    }
    Ok(Handle(store))
}

fn buffer_or_error(result: Result<Vec<u8>, NativeError>) -> Either<Buffer, NativeError> {
    match result {
        Ok(bytes) => Either::A(Buffer::from(bytes)),
        Err(err) => Either::B(err),
    }
}
