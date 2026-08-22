//! Narrow Win32 ABI for the modern folder dialog and cross-thread close.
//!
//! Every raw pointer remains inside this crate. COM references and task
//! allocator strings are paired by RAII, initialization is balanced on all
//! post-success paths, and callers receive only owned UTF-8 strings/errors.

#[cfg(windows)]
mod windows;

/// Runs the modal modern Windows folder picker on the calling thread.
///
/// `on_showing` receives the native thread id immediately before `Show`, so a
/// different process thread can post `WM_CLOSE` during caller cancellation.
///
/// # Errors
///
/// Returns unsupported-platform, COM initialization/creation, HRESULT, or
/// result extraction failures.
pub fn run_folder_dialog(
    title: &str,
    on_showing: impl FnOnce(u32),
) -> anyhow::Result<Option<String>> {
    #[cfg(windows)]
    {
        windows::run_folder_dialog(title, on_showing)
    }
    #[cfg(not(windows))]
    {
        let _ = (title, on_showing);
        anyhow::bail!("Win32 folder dialog is unavailable on this platform")
    }
}

/// Posts `WM_CLOSE` to every top-level window owned by `thread_id`.
///
/// # Errors
///
/// The current Win32 API does not surface per-window post failures, matching
/// the source adapter's best-effort retry contract. Non-Windows builds return
/// an unsupported-platform failure.
pub fn close_thread_windows(thread_id: u32) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows::close_thread_windows(thread_id);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = thread_id;
        anyhow::bail!("Win32 window closing is unavailable on this platform")
    }
}
