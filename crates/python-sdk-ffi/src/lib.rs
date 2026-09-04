//! Allocator-owned C ABI for Python compatibility bindings.
//!
//! A C calling convention cannot represent Rust borrows or callable trait objects.
//! The only unsafe operations here borrow caller-provided bytes and invoke its
//! callback. Returned buffers remain Rust-owned behind non-reused handles;
//! releasing a stale handle cannot free a later allocation at the same address.
//! SDK decisions are delegated to the safe seekdeep-python-sdk crate. No Python
//! ABI symbols or Python object pointers are linked into the native library.

mod client;
mod dispatch;
mod objects;

use std::{
    any::Any,
    collections::HashMap,
    ffi::CString,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_python_sdk::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Version of the byte-buffer and callback ABI.
pub const ABI_VERSION: u32 = seekdeep_python_sdk::bindings::ABI_VERSION;

/// Opaque interpreter invocation or callback-owner identity.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContextId(pub u64);

/// Callback receiving UTF-8 JSON and returning a buffer from this library's copy entry.
pub type CallbackFunction = unsafe extern "C" fn(u64, *const u8, usize) -> u64;

#[derive(Clone, Copy)]
pub(crate) struct Callback {
    function: Option<CallbackFunction>,
    context: ContextId,
}

pub(crate) struct Reply {
    value: Value,
    retained: Vec<Box<dyn Any + Send + Sync>>,
}

impl Reply {
    fn json(value: Value) -> Self {
        let mut reply = json!({"kind":"json"});
        reply["value"] = value;
        Self {
            value: reply,
            retained: Vec::new(),
        }
    }

    fn object<T: Any + Send + Sync>(
        handle: seekdeep_python_sdk::ObjectHandle,
        retained: T,
    ) -> Self {
        Self {
            value: json!({"kind":"object","value":handle}),
            retained: vec![Box::new(retained)],
        }
    }
}

struct Buffer {
    bytes: CString,
    _retained: Vec<Box<dyn Any + Send + Sync>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BufferId(u64);

static BUFFERS: LazyLock<Mutex<HashMap<BufferId, Buffer>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_BUFFER: AtomicU64 = AtomicU64::new(1);

fn allocate(bytes: Vec<u8>, retained: Vec<Box<dyn Any + Send + Sync>>) -> u64 {
    let Ok(bytes) = CString::new(bytes) else {
        return 0;
    };
    let Ok(handle) =
        NEXT_BUFFER.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
    else {
        return 0;
    };
    BUFFERS.lock().insert(
        BufferId(handle),
        Buffer {
            bytes,
            _retained: retained,
        },
    );
    handle
}

fn take(handle: u64) -> Option<Buffer> {
    BUFFERS.lock().remove(&BufferId(handle))
}

impl Callback {
    fn with_owner(self, owner: u64) -> Self {
        Self {
            context: ContextId(owner),
            ..self
        }
    }
    pub(crate) fn invoke(self, operation: &str, arguments: Value) -> Result<Value> {
        let function = self.function.ok_or_else(|| {
            Error::new(
                ErrorKind::Value,
                "this SDK operation requires an interpreter callback",
            )
        })?;
        let mut payload = json!({"operation":operation});
        payload["arguments"] = arguments;
        let request = serde_json::to_vec(&payload)
            .map_err(|error| Error::new(ErrorKind::Value, error.to_string()))?;
        // SAFETY: The caller keeps its callback alive for this invocation. The
        // borrowed request bytes remain initialized and immutable until it returns.
        let handle = unsafe { function(self.context.0, request.as_ptr(), request.len()) };
        let buffer = take(handle).ok_or_else(|| {
            Error::new(
                ErrorKind::Value,
                "interpreter callback returned an unowned buffer",
            )
        })?;
        let response: Value = serde_json::from_slice(buffer.bytes.as_bytes()).map_err(|error| {
            Error::new(
                ErrorKind::Value,
                format!("invalid callback response: {error}"),
            )
        })?;
        if let Some(error) = response.get("error") {
            let mut error: Error = serde_json::from_value(error.clone()).map_err(|error| {
                Error::new(
                    ErrorKind::Value,
                    format!("invalid callback exception: {error}"),
                )
            })?;
            if let Some(id) = error.exception {
                error.exception_owner = Some(seekdeep_python_sdk::ExceptionOwnerId(self.context.0));
                let object =
                    objects::Object::new(self, json!({"owner":self.context.0,"value":id.0}))?;
                error.retained = Some(seekdeep_python_sdk::Retained::new(object));
            }
            return Err(error);
        }
        response.get("ok").cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::Value,
                "interpreter callback response has no result",
            )
        })
    }
}

fn encode_result(result: Result<Reply>) -> u64 {
    let (value, retained): (Value, Vec<Box<dyn Any + Send + Sync>>) = match result {
        Ok(reply) => (json!({"ok":reply.value}), reply.retained),
        Err(error) => (json!({"error":error}), vec![Box::new(error)]),
    };
    match serde_json::to_vec(&value) {
        Ok(bytes) => allocate(bytes, retained),
        Err(_) => 0,
    }
}

unsafe fn borrowed<'a>(bytes: *const u8, length: usize) -> Result<&'a [u8]> {
    if length == 0 {
        return Ok(&[]);
    }
    if bytes.is_null() || length > isize::MAX as usize {
        return Err(Error::new(ErrorKind::Value, "invalid SDK input buffer"));
    }
    // SAFETY: The exported entry requires a readable, initialized allocation
    // of length bytes for its complete call. Null and oversized inputs were rejected.
    Ok(unsafe { std::slice::from_raw_parts(bytes, length) })
}

/// Reports the calling-convention version without initializing any SDK state.
#[unsafe(no_mangle)]
pub extern "C" fn seekdeep_python_sdk_abi_version() -> u32 {
    ABI_VERSION
}

/// Executes one Rust SDK operation and returns an owned JSON-buffer handle.
///
/// # Safety
/// A nonempty input must point to length readable initialized bytes for this call.
/// The callback must remain callable until every native handle retaining it is
/// released and its reader threads have stopped. It may be invoked by later
/// operations or handle destruction on another thread, and must return only
/// buffers allocated by the copy entry.
/// The caller frees the returned buffer with this library's free entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn seekdeep_python_sdk_call(
    bytes: *const u8,
    length: usize,
    callback: Option<CallbackFunction>,
    context: u64,
) -> u64 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: This entry forwards its documented input-buffer contract.
        let bytes = unsafe { borrowed(bytes, length) }?;
        dispatch::run(
            bytes,
            Callback {
                function: callback,
                context: ContextId(context),
            },
        )
    }));
    encode_result(result.unwrap_or_else(|_| {
        Err(Error::new(
            ErrorKind::Value,
            "native SDK operation panicked",
        ))
    }))
}

/// Copies callback JSON into a library-owned buffer and returns its handle.
///
/// # Safety
/// A nonempty input must point to length readable initialized bytes for this call.
/// Ownership of the returned handle transfers to the consuming native callback,
/// or to the caller until it invokes the free entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn seekdeep_python_sdk_copy(bytes: *const u8, length: usize) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: This entry forwards its documented input-buffer contract.
        unsafe { borrowed(bytes, length) }.map_or(0, |bytes| allocate(bytes.to_vec(), Vec::new()))
    }))
    .unwrap_or(0)
}

/// Borrows read-only buffer bytes until the owning handle is released or consumed.
///
/// An unknown handle returns null. The caller must not free the handle while reading.
#[unsafe(no_mangle)]
pub extern "C" fn seekdeep_python_sdk_buffer_data(handle: u64) -> *const u8 {
    BUFFERS
        .lock()
        .get(&BufferId(handle))
        .map_or(std::ptr::null(), |buffer| buffer.bytes.as_ptr().cast())
}

/// Returns the buffer's byte length, excluding its trailing NUL; unknown handles return zero.
#[unsafe(no_mangle)]
pub extern "C" fn seekdeep_python_sdk_buffer_length(handle: u64) -> usize {
    BUFFERS
        .lock()
        .get(&BufferId(handle))
        .map_or(0, |buffer| buffer.bytes.as_bytes().len())
}

/// Releases a library-owned buffer; zero, unknown, and already-released handles are inert.
/// Retained foreign objects may call their original callback while being released.
#[unsafe(no_mangle)]
pub extern "C" fn seekdeep_python_sdk_free(handle: u64) {
    let buffer = take(handle);
    let _ = catch_unwind(AssertUnwindSafe(|| drop(buffer)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_buffers_round_trip_and_free_is_idempotent() {
        let request = br#"{"op":"about"}"#;
        // SAFETY: Static bytes satisfy the input contract; about uses no callback.
        let handle = unsafe { seekdeep_python_sdk_call(request.as_ptr(), request.len(), None, 0) };
        let buffer = take(handle).unwrap();
        let response: Value = serde_json::from_slice(buffer.bytes.as_bytes()).unwrap();
        assert_eq!(response["ok"]["value"]["abiVersion"], ABI_VERSION);
        let later = allocate(b"later".to_vec(), Vec::new());
        seekdeep_python_sdk_free(handle);
        seekdeep_python_sdk_free(handle);
        seekdeep_python_sdk_free(0);
        assert_eq!(seekdeep_python_sdk_buffer_length(later), 5);
        seekdeep_python_sdk_free(later);
    }

    #[test]
    fn invalid_input_is_a_typed_error_without_dereferencing_null() {
        // SAFETY: This entry explicitly rejects null nonempty inputs before dereference.
        let handle = unsafe { seekdeep_python_sdk_call(std::ptr::null(), 1, None, 0) };
        let buffer = take(handle).unwrap();
        let value: Value = serde_json::from_slice(buffer.bytes.as_bytes()).unwrap();
        assert_eq!(value["error"]["kind"], "value");
    }
}
