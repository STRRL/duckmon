//! C ABI for duckmon. The header `include/duckmon.h` is generated from this
//! file by cbindgen.
//!
//! Every function returns a `DUCKMON_*` code and saves a message for the
//! calling thread, read with `duckmon_last_error`. Events go in as JSON Lines
//! text and query results come out as an Arrow IPC stream.

mod arrow_ipc;
mod error;
mod handle;
mod json;

use std::ffi::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

use crate::arrow_ipc::arrow_ipc_stream;
use crate::error::{CallError, call, last_error_pointer};
use crate::handle::{optional_text_argument, store_argument, text_argument};
pub use crate::handle::{Bytes, StoreHandle};
use crate::handle::bytes_from_vec;
use crate::json::{compact_options_from_json, compact_report_json, events_from_json_lines, options_from_json};

/// The ABI version of this header. A binding compares it with `duckmon_abi_version()`
/// when it loads and refuses to run when they differ.
pub const DUCKMON_ABI_VERSION: u32 = 1;

/// The call succeeded.
pub const DUCKMON_OK: i32 = 0;
/// A pointer is NULL, a text is not UTF 8 or holds a NUL, or the options JSON is wrong.
pub const DUCKMON_ERROR_INVALID_ARGUMENT: i32 = 1;
/// A line of the events text is wrong, or the store refused an event.
pub const DUCKMON_ERROR_INVALID_EVENT: i32 = 2;
/// The store directory path cannot be used.
pub const DUCKMON_ERROR_INVALID_DIRECTORY: i32 = 3;
/// DuckDB reported an error, for example for bad SQL.
pub const DUCKMON_ERROR_DATABASE: i32 = 4;
/// A file or directory operation failed.
pub const DUCKMON_ERROR_FILE: i32 = 5;
/// The store is closed or its writer thread has stopped.
pub const DUCKMON_ERROR_CLOSED: i32 = 6;
/// A merged file did not hold the same rows as its inputs.
pub const DUCKMON_ERROR_COMPACT: i32 = 7;
/// duckmon panicked, or reported an error kind this ABI version does not know.
pub const DUCKMON_ERROR_INTERNAL: i32 = 8;

/// DuckDB registers its compiled in extensions from a static constructor. A C
/// program linked with `libduckmon_ffi.a` drops that constructor, because
/// nothing refers to it, so duckmon registers them itself before DuckDB starts.
fn register_linked_extensions() {
    // Defined by DuckDB. Registering again does nothing. Declared here, inside
    // the function, so that cbindgen leaves it out of the header.
    unsafe extern "C" {
        fn duckdb_register_static_extensions() -> i32;
    }
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // DuckDB keeps any registration error and reports it when a database opens.
        // SAFETY: the function takes no arguments and is safe to call at any time.
        let _ = unsafe { duckdb_register_static_extensions() };
    });
}

/// Returns `DUCKMON_ABI_VERSION` of the library that is actually loaded. Never fails.
#[unsafe(no_mangle)]
pub extern "C" fn duckmon_abi_version() -> u32 {
    DUCKMON_ABI_VERSION
}

/// The message of the last failed duckmon call on this thread, or "" when that
/// call succeeded or no call was made yet.
///
/// The library owns the text: never free it. It stays valid until the next
/// duckmon call on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn duckmon_last_error() -> *const c_char {
    last_error_pointer()
}

/// Opens the store in `dir` and creates `dir` when it is missing.
///
/// `options_json` may be NULL for the defaults, or a JSON object with the
/// optional keys `flush_interval_ms`, `channel_capacity` (from 1 to 1000000)
/// and `resource`.
/// Sets `*out_store` to NULL first and to the new handle on success. The
/// caller frees the handle with `duckmon_store_free`. Blocks while DuckDB starts.
///
/// # Safety
///
/// `dir` and `options_json` must be NULL or NUL terminated strings.
/// `out_store` must be NULL or point to writable memory for one pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckmon_open(
    dir: *const c_char,
    options_json: *const c_char,
    out_store: *mut *mut StoreHandle,
) -> i32 {
    call(|| {
        let out_store = unsafe { out_store.as_mut() }
            .ok_or_else(|| CallError::invalid_argument("out_store is NULL"))?;
        *out_store = std::ptr::null_mut();
        let dir = unsafe { text_argument(dir, "dir") }?;
        let options_text = unsafe { optional_text_argument(options_json, "options_json") }?;
        let options = options_from_json(options_text).map_err(CallError::invalid_argument)?;
        register_linked_extensions();
        let store = duckmon::open(dir, options)?;
        *out_store = Box::into_raw(Box::new(StoreHandle::new(store)));
        Ok(())
    })
}

/// Adds events, given as JSON Lines text: one JSON object per line with the
/// keys `ts` (required, nanoseconds since 1970 UTC), `name` (required, not
/// empty), `message`, `duration_ns`, `trace_id`, `span_id`, `parent_id` and
/// `attrs` (a JSON object). Unknown keys are refused. Empty lines are skipped.
///
/// Every line is checked first. When one is wrong, nothing is written and the
/// message names the line. Events reach a Parquet file at the next flush.
/// Blocks when the writer thread has too many batches waiting.
///
/// # Safety
///
/// `store` must be NULL or a handle from `duckmon_open` that was not freed.
/// `events_json_lines` must be NULL or a NUL terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckmon_store_write_events(
    store: *mut StoreHandle,
    events_json_lines: *const c_char,
) -> i32 {
    call(|| {
        let handle = unsafe { store_argument(store) }?;
        let text = unsafe { text_argument(events_json_lines, "events_json_lines") }?;
        let events = events_from_json_lines(text).map_err(CallError::invalid_event)?;
        handle.with_store(|store| store.write_events(&events))
    })
}

/// Waits until every event written before this call is in a finished Parquet
/// file. Also returns any error the writer thread saved since the last flush.
///
/// # Safety
///
/// `store` must be NULL or a handle from `duckmon_open` that was not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckmon_store_flush(store: *mut StoreHandle) -> i32 {
    call(|| {
        let handle = unsafe { store_argument(store) }?;
        handle.with_store(|store| store.flush())
    })
}

/// Runs SQL against the view `events` and returns the result of the last
/// statement as an Arrow IPC stream: the schema first, then at least one batch,
/// which may have zero rows.
///
/// Sets `*out_arrow_ipc` to {NULL, 0} first and to the result on success. The
/// caller frees it with `duckmon_bytes_free`. Blocks until the writer thread has
/// handled every write made before this call, then while the query runs.
///
/// # Safety
///
/// `store` must be NULL or a handle from `duckmon_open` that was not freed.
/// `sql` must be NULL or a NUL terminated string.
/// `out_arrow_ipc` must be NULL or point to a writable `duckmon_bytes`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckmon_store_query(
    store: *mut StoreHandle,
    sql: *const c_char,
    out_arrow_ipc: *mut Bytes,
) -> i32 {
    call(|| {
        let out_arrow_ipc = unsafe { out_arrow_ipc.as_mut() }
            .ok_or_else(|| CallError::invalid_argument("out_arrow_ipc is NULL"))?;
        *out_arrow_ipc = Bytes::empty();
        let handle = unsafe { store_argument(store) }?;
        let sql = unsafe { text_argument(sql, "sql") }?;
        let batches = handle.with_store(|store| store.query(sql))?;
        *out_arrow_ipc = bytes_from_vec(arrow_ipc_stream(&batches)?);
        Ok(())
    })
}

/// Writes the buffer one last time, stops the writer thread and returns the
/// error of that last flush.
///
/// Calls that run at the same time either finish first or return
/// `DUCKMON_ERROR_CLOSED`. Every later call on this handle, a second close
/// included, returns `DUCKMON_ERROR_CLOSED`. The handle still needs
/// `duckmon_store_free`.
///
/// # Safety
///
/// `store` must be NULL or a handle from `duckmon_open` that was not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckmon_store_close(store: *mut StoreHandle) -> i32 {
    call(|| {
        let handle = unsafe { store_argument(store) }?;
        handle.close()
    })
}

/// Frees a handle. When it is not closed yet, closes it first and ignores
/// errors, so it blocks like `duckmon_store_close`. NULL does nothing.
///
/// # Safety
///
/// `store` must be NULL or a handle from `duckmon_open` that was not freed.
/// No other call may use the handle at the same time or afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckmon_store_free(store: *mut StoreHandle) {
    if store.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(store) })));
}

/// Merges the small files of past days and deletes old days.
///
/// `options_json` may be NULL for the defaults, or a JSON object with the
/// optional keys `keep_days`, `settle_time_ms` and `dry_run`. Without
/// `keep_days` nothing is deleted.
/// Sets `*out_report_json` to {NULL, 0} first and on success to UTF 8 JSON
/// text, with no NUL at the end, of the form {"actions":[...]}. The caller
/// frees it with `duckmon_bytes_free`. Blocks while it waits for the
/// compaction lock and merges files.
///
/// # Safety
///
/// `dir` and `options_json` must be NULL or NUL terminated strings.
/// `out_report_json` must be NULL or point to a writable `duckmon_bytes`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckmon_compact(
    dir: *const c_char,
    options_json: *const c_char,
    out_report_json: *mut Bytes,
) -> i32 {
    call(|| {
        let out_report_json = unsafe { out_report_json.as_mut() }
            .ok_or_else(|| CallError::invalid_argument("out_report_json is NULL"))?;
        *out_report_json = Bytes::empty();
        let dir = unsafe { text_argument(dir, "dir") }?;
        let options_text = unsafe { optional_text_argument(options_json, "options_json") }?;
        let options = compact_options_from_json(options_text).map_err(CallError::invalid_argument)?;
        register_linked_extensions();
        let report = duckmon::compact(dir, &options)?;
        *out_report_json = bytes_from_vec(compact_report_json(&report).into_bytes());
        Ok(())
    })
}

/// Frees bytes that duckmon handed out and sets them to {NULL, 0}. A NULL
/// pointer or NULL `data` does nothing.
///
/// # Safety
///
/// `bytes` must be NULL or point to a `duckmon_bytes` that duckmon filled and
/// that was not freed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckmon_bytes_free(bytes: *mut Bytes) {
    let Some(bytes) = (unsafe { bytes.as_mut() }) else {
        return;
    };
    if bytes.data.is_null() {
        return;
    }
    let slice = std::ptr::slice_from_raw_parts_mut(bytes.data, bytes.len);
    let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(slice) })));
    *bytes = Bytes::empty();
}
