use std::cell::RefCell;
use std::ffi::CString;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{
    DUCKMON_ERROR_CLOSED, DUCKMON_ERROR_COMPACT, DUCKMON_ERROR_DATABASE, DUCKMON_ERROR_FILE,
    DUCKMON_ERROR_INTERNAL, DUCKMON_ERROR_INVALID_ARGUMENT, DUCKMON_ERROR_INVALID_DIRECTORY,
    DUCKMON_ERROR_INVALID_EVENT, DUCKMON_OK,
};

/// Why one ABI call failed: a `DUCKMON_ERROR_*` code and a message for people.
#[derive(Debug)]
pub struct CallError {
    pub code: i32,
    pub message: String,
}

impl CallError {
    pub fn invalid_argument(message: impl Into<String>) -> CallError {
        CallError {
            code: DUCKMON_ERROR_INVALID_ARGUMENT,
            message: message.into(),
        }
    }

    pub fn invalid_event(message: impl Into<String>) -> CallError {
        CallError {
            code: DUCKMON_ERROR_INVALID_EVENT,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> CallError {
        CallError {
            code: DUCKMON_ERROR_INTERNAL,
            message: message.into(),
        }
    }

    pub fn closed() -> CallError {
        CallError::from(duckmon::Error::Closed)
    }
}

impl From<duckmon::Error> for CallError {
    fn from(err: duckmon::Error) -> Self {
        let code = match &err {
            duckmon::Error::InvalidEvent(_) => DUCKMON_ERROR_INVALID_EVENT,
            duckmon::Error::InvalidDirectory(_) => DUCKMON_ERROR_INVALID_DIRECTORY,
            duckmon::Error::Database(_) => DUCKMON_ERROR_DATABASE,
            duckmon::Error::File(_) => DUCKMON_ERROR_FILE,
            duckmon::Error::Closed => DUCKMON_ERROR_CLOSED,
            duckmon::Error::Compact(_) => DUCKMON_ERROR_COMPACT,
            _ => DUCKMON_ERROR_INTERNAL,
        };
        CallError {
            code,
            message: err.to_string(),
        }
    }
}

thread_local! {
    /// The message of the last failed call on this thread, or empty after a call that succeeded.
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

/// Pointer to the last error message of this thread. Valid until the next call on this thread.
pub fn last_error_pointer() -> *const std::ffi::c_char {
    LAST_ERROR.with(|last_error| last_error.borrow().as_ptr())
}

/// Runs the body of one ABI call, catches a panic, saves the error message
/// for this thread and returns the code.
pub fn call(body: impl FnOnce() -> Result<(), CallError>) -> i32 {
    let result = match catch_unwind(AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(payload) => Err(CallError::internal(format!(
            "duckmon panicked: {}",
            panic_text(payload.as_ref())
        ))),
    };
    let (code, message) = match result {
        Ok(()) => (DUCKMON_OK, String::new()),
        Err(err) => (err.code, err.message),
    };
    let message = CString::new(message.replace('\0', "")).unwrap_or_default();
    LAST_ERROR.with(|last_error| *last_error.borrow_mut() = message);
    code
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(text) = payload.downcast_ref::<&str>() {
        text
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text
    } else {
        "unknown panic"
    }
}
