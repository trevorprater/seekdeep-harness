//! Typed failures transported without conflating Python exceptions and JSON-RPC errors.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// An owned foreign resource retained for the lifetime of a native exception.
#[derive(Clone)]
pub struct Retained(Arc<dyn std::any::Any + Send + Sync>);

impl Retained {
    /// Retains a resource whose destructor releases its foreign reference.
    pub fn new<T: std::any::Any + Send + Sync>(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl std::fmt::Debug for Retained {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Retained")
            .field("owners", &Arc::strong_count(&self.0))
            .finish()
    }
}

/// Opaque exception retained by the foreign-language callback owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExceptionId(pub u64);

/// Interpreter context that retains a foreign exception identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExceptionOwnerId(pub u64);

/// Exception category exposed by the synchronous SDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The subprocess is unavailable or its standard streams closed.
    TransportClosed,
    /// A turn-level protocol requirement was violated.
    Protocol,
    /// The peer returned a JSON-RPC error object.
    JsonRpc,
    /// A caller or peer value has the wrong type.
    Type,
    /// A caller value is unsupported.
    Value,
    /// A timeout cannot be represented by the host's native wait primitive.
    Overflow,
    /// A required package or artifact is absent.
    FileNotFound,
    /// Native filesystem or process failure, retaining errno and filename.
    Os,
    /// A request exhausted its configured wait interval.
    Timeout,
    /// A direct process wait exhausted its timeout.
    SubprocessTimeout,
    /// A standard stream contains invalid UTF-8.
    UnicodeDecode,
    /// A nonblocking queue read found no item.
    Empty,
    /// An exact exception object retained by a foreign callback.
    Foreign,
}

/// Shared exception information with a pointer-sized result representation.
#[derive(Clone, Debug)]
pub struct Error(Arc<ErrorDetails>);

/// Protocol and operating-system metadata carried by one exception.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorDetails {
    /// Public exception category.
    pub kind: ErrorKind,
    /// Exact user-facing message without a stack trace.
    pub message: String,
    /// JSON-RPC numeric code; Python also accepts booleans as integers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
    /// JSON-RPC error data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Native operating-system error number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errno: Option<i32>,
    /// Filename attached to an operating-system failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Retained foreign exception identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception: Option<ExceptionId>,
    /// Owner of the retained exception, independent of the current ABI caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_owner: Option<ExceptionOwnerId>,
    /// Whether the foreign exception derives from Python's import-error base class.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub import_error: bool,
    /// Explicit exception cause at a native/foreign boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<Box<Error>>,
    /// Keeps an interpreter exception alive until the final native copy is released.
    #[serde(skip)]
    pub retained: Option<Retained>,
}

impl std::ops::Deref for Error {
    type Target = ErrorDetails;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Error {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl Serialize for Error {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Error {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        ErrorDetails::deserialize(deserializer).map(|details| Self(Arc::new(details)))
    }
}

impl Error {
    /// Creates a typed failure with no protocol or operating-system metadata.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self(Arc::new(ErrorDetails {
            kind,
            message: message.into(),
            code: None,
            data: None,
            errno: None,
            filename: None,
            exception: None,
            exception_owner: None,
            import_error: false,
            cause: None,
            retained: None,
        }))
    }

    /// Preserves native errno and filename for Python's operating-system exception constructor.
    pub fn io(error: &std::io::Error, filename: Option<String>) -> Self {
        let mut failure = Self::new(ErrorKind::Os, error.to_string());
        failure.errno = error.raw_os_error();
        failure.filename = filename;
        if let Some(errno) = failure.errno {
            let suffix = format!(" (os error {errno})");
            if let Some(message) = failure.message.strip_suffix(&suffix).map(str::to_owned) {
                failure.message = message;
            }
        }
        failure
    }

    /// Records a causal exception without changing the public category or message.
    #[must_use]
    pub fn caused_by(mut self, cause: Self) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Result used by the native synchronous SDK and its binding adapters.
pub type Result<T> = std::result::Result<T, Error>;
