//! Fail-closed Win32 error identity.

/// One checked Win32 API failure with its exact machine code.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{api} failed (Win32 {win32_code}){rendered_detail}")]
pub struct Win32Error {
    /// Failing Win32 API name.
    pub api: String,
    /// `GetLastError` or HRESULT-style API return.
    pub win32_code: u32,
    rendered_detail: String,
    detail: Option<String>,
}

impl Win32Error {
    /// Creates the exact source-compatible diagnostic.
    #[must_use]
    pub fn new(api: impl Into<String>, win32_code: u32, detail: Option<String>) -> Self {
        let api = api.into();
        let rendered_detail = detail
            .as_ref()
            .map_or_else(String::new, |detail| format!(": {detail}"));
        Self {
            api,
            win32_code,
            rendered_detail,
            detail,
        }
    }

    /// Optional detail without the punctuation used by Display.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}
