use std::ffi::{CStr, CString};

use duckmon_ffi::{
    Bytes, DUCKMON_ERROR_INVALID_ARGUMENT, DUCKMON_OK, StoreHandle, duckmon_bytes_free, duckmon_last_error,
    duckmon_store_free,
};
use napi::bindgen_prelude::{AsyncTask, ToNapiValue, TypeName};
use napi::{Env, Task};
use napi_derive::napi;

/// A failed duckmon call: the `DUCKMON_ERROR_*` code and its message.
#[napi(object)]
pub struct NativeError {
    pub code: i32,
    pub message: String,
}

/// Owns a store handle and frees it when the last user is gone.
pub struct Handle(pub *mut StoreHandle);

// SAFETY: the C ABI promises that one handle may be used from many threads at once.
unsafe impl Send for Handle {}
// SAFETY: same promise as for Send.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this is the last owner, so no other call uses the handle now or later.
        unsafe { duckmon_store_free(self.0) };
    }
}

/// Turns a duckmon return code into `None` or the error. Must run on the
/// thread that made the call, because the message is kept per thread.
pub fn check(code: i32) -> Option<NativeError> {
    if code == DUCKMON_OK {
        return None;
    }
    // SAFETY: duckmon_last_error never returns NULL, and the text stays valid
    // until the next duckmon call on this thread, after the copy below.
    let message = unsafe { CStr::from_ptr(duckmon_last_error()) };
    Some(NativeError {
        code,
        message: message.to_string_lossy().into_owned(),
    })
}

/// Turns a JavaScript string into C text. A NUL inside gives an `INVALID_ARGUMENT` error.
pub fn c_text(text: String, what: &str) -> Result<CString, NativeError> {
    CString::new(text).map_err(|_| NativeError {
        code: DUCKMON_ERROR_INVALID_ARGUMENT,
        message: format!("{what} holds a NUL character"),
    })
}

/// Runs a call that fills `Bytes`, copies them into a vector and frees them.
pub fn bytes_result(body: impl FnOnce(&mut Bytes) -> i32) -> Result<Vec<u8>, NativeError> {
    let mut bytes = Bytes {
        data: std::ptr::null_mut(),
        len: 0,
    };
    let code = body(&mut bytes);
    if let Some(err) = check(code) {
        return Err(err);
    }
    let copy = if bytes.data.is_null() {
        Vec::new()
    } else {
        // SAFETY: on success duckmon filled `data` with `len` bytes that live until duckmon_bytes_free.
        unsafe { std::slice::from_raw_parts(bytes.data, bytes.len) }.to_vec()
    };
    // SAFETY: duckmon filled `bytes`, and they are freed only here.
    unsafe { duckmon_bytes_free(&mut bytes) };
    Ok(copy)
}

/// Runs one blocking call on the libuv thread pool and resolves the promise
/// with its value. A duckmon failure is part of the value, so the code is kept.
pub struct NativeTask<T> {
    body: Option<Box<dyn FnOnce() -> T + Send>>,
}

impl<T> NativeTask<T> {
    pub fn start(body: impl FnOnce() -> T + Send + 'static) -> AsyncTask<NativeTask<T>>
    where
        NativeTask<T>: Task,
    {
        AsyncTask::new(NativeTask {
            body: Some(Box::new(body)),
        })
    }
}

impl<T: ToNapiValue + TypeName + Send + 'static> Task for NativeTask<T> {
    type Output = T;
    type JsValue = T;

    fn compute(&mut self) -> napi::Result<T> {
        let body = self
            .body
            .take()
            .ok_or_else(|| napi::Error::from_reason("duckmon task ran twice"))?;
        Ok(body())
    }

    fn resolve(&mut self, _env: Env, output: T) -> napi::Result<T> {
        Ok(output)
    }
}
