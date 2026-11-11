use std::ffi::{CStr, c_char};
use std::sync::{PoisonError, RwLock};

use crate::error::CallError;

/// An open store. C sees only a pointer to it.
pub struct StoreHandle {
    store: RwLock<Option<duckmon::Store>>,
}

const fn shared_between_threads<T: Send + Sync>() {}

impl StoreHandle {
    pub fn new(store: duckmon::Store) -> StoreHandle {
        // The C ABI promises that one handle may be used by many threads at once.
        // This fails to compile when `duckmon::Store` stops being safe to share.
        const { shared_between_threads::<StoreHandle>() };
        StoreHandle {
            store: RwLock::new(Some(store)),
        }
    }

    /// Runs `body` with the store under the read lock, or returns `CLOSED` when it was closed.
    pub fn with_store<T>(
        &self,
        body: impl FnOnce(&duckmon::Store) -> duckmon::Result<T>,
    ) -> Result<T, CallError> {
        let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
        let store = store.as_ref().ok_or_else(CallError::closed)?;
        Ok(body(store)?)
    }

    /// Closes the store while holding the write lock, so calls that race with
    /// close wait for it and then see `CLOSED`.
    pub fn close(&self) -> Result<(), CallError> {
        let mut store = self.store.write().unwrap_or_else(PoisonError::into_inner);
        let store = store.take().ok_or_else(CallError::closed)?;
        Ok(store.close()?)
    }
}

/// Bytes that duckmon hands to the caller. Free them with `duckmon_bytes_free`.
#[repr(C)]
pub struct Bytes {
    /// Start of the bytes, or NULL when there are none.
    pub data: *mut u8,
    /// Number of bytes.
    pub len: usize,
}

impl Bytes {
    pub fn empty() -> Bytes {
        Bytes {
            data: std::ptr::null_mut(),
            len: 0,
        }
    }
}

/// Hands a vector to the caller. The boxed slice has `len == capacity`, so
/// `data` and `len` are enough to free it later.
pub fn bytes_from_vec(bytes: Vec<u8>) -> Bytes {
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    Bytes {
        data: Box::into_raw(boxed).cast::<u8>(),
        len,
    }
}

/// Reads a borrowed C text argument. `what` names the argument in the error message.
///
/// # Safety
///
/// `text` must be NULL or point to a NUL terminated string that lives during the call.
pub unsafe fn text_argument<'a>(text: *const c_char, what: &str) -> Result<&'a str, CallError> {
    if text.is_null() {
        return Err(CallError::invalid_argument(format!("{what} is NULL")));
    }
    let text = unsafe { CStr::from_ptr(text) };
    text.to_str()
        .map_err(|err| CallError::invalid_argument(format!("{what} is not UTF-8: {err}")))
}

/// Like `text_argument`, but NULL gives `None`.
///
/// # Safety
///
/// Same as `text_argument`.
pub unsafe fn optional_text_argument<'a>(
    text: *const c_char,
    what: &str,
) -> Result<Option<&'a str>, CallError> {
    if text.is_null() {
        return Ok(None);
    }
    unsafe { text_argument(text, what) }.map(Some)
}

/// Reads the store handle argument.
///
/// # Safety
///
/// `store` must be NULL or a handle from `duckmon_open` that was not freed.
pub unsafe fn store_argument<'a>(store: *const StoreHandle) -> Result<&'a StoreHandle, CallError> {
    unsafe { store.as_ref() }.ok_or_else(|| CallError::invalid_argument("store is NULL"))
}
