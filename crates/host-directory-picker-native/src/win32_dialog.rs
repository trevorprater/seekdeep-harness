//! Win32 folder-dialog sequencing and abortable worker driver.

use std::{
    fmt,
    io::{BufRead as _, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::future::BoxFuture;
use seekdeep_llm::AbortSignal;
use tokio::sync::mpsc;

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`.
pub const HRESULT_CANCELLED: i32 = 0x8007_04c7_u32.cast_signed();
/// `FOS_PICKFOLDERS`.
pub const FOS_PICKFOLDERS: u32 = 0x20;
/// `FOS_FORCEFILESYSTEM`.
pub const FOS_FORCEFILESYSTEM: u32 = 0x40;
/// `FOS_NOCHANGEDIR`.
pub const FOS_NOCHANGEDIR: u32 = 0x8;
/// Dialog title on every platform.
pub const DIALOG_TITLE: &str = "Select Workspace Directory";
/// Default `WM_CLOSE` retry cadence.
pub const CLOSE_RETRY: Duration = Duration::from_millis(150);
/// Retry budget before the worker is killed.
pub const CLOSE_MAX_ATTEMPTS: usize = 20;

/// One COM folder dialog.
pub trait Win32FolderDialog {
    /// Applies `FOS_*` options.
    fn set_options(&mut self, options: u32) -> i32;
    /// Sets the operator-facing title.
    fn set_title(&mut self, title: &str) -> i32;
    /// Blocks until selection or dismissal.
    fn show(&mut self) -> i32;
    /// Extracts the selected filesystem path.
    fn result_path(&mut self) -> (i32, Option<String>);
    /// Releases the COM reference.
    fn release(&mut self);
}

/// Thread-local Win32/COM bindings.
pub trait Win32DialogBindings {
    /// Best-effort thread DPI opt-in.
    fn set_thread_dpi_awareness(&mut self);
    /// Initializes STA COM.
    fn co_initialize_sta(&mut self) -> i32;
    /// Balances successful COM initialization.
    fn co_uninitialize(&mut self);
    /// Creates `IFileOpenDialog`.
    ///
    /// # Errors
    ///
    /// Returns a native library, COM activation, or pointer failure.
    fn create_folder_dialog(&mut self) -> anyhow::Result<Box<dyn Win32FolderDialog>>;
    /// Native thread id used by the abort driver.
    fn current_thread_id(&self) -> u32;
}

/// Runs one blocking folder-dialog COM conversation with exact cleanup.
///
/// # Errors
///
/// Returns the first failing HRESULT or binding creation failure.
pub fn run_folder_dialog(
    bindings: &mut dyn Win32DialogBindings,
    title: &str,
    on_showing: impl FnOnce(u32),
) -> anyhow::Result<Option<String>> {
    bindings.set_thread_dpi_awareness();
    check(bindings.co_initialize_sta(), "CoInitializeEx")?;
    let result = match bindings.create_folder_dialog() {
        Ok(mut dialog) => {
            let result = (|| {
                check(
                    dialog.set_options(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR),
                    "SetOptions",
                )?;
                check(dialog.set_title(title), "SetTitle")?;
                on_showing(bindings.current_thread_id());
                let shown = dialog.show();
                if shown == HRESULT_CANCELLED {
                    return Ok(None);
                }
                check(shown, "Show")?;
                let (result, path) = dialog.result_path();
                check(result, "GetResult")?;
                path.ok_or_else(|| anyhow::anyhow!("GetResult succeeded without a filesystem path"))
                    .map(Some)
            })();
            dialog.release();
            result
        }
        Err(error) => Err(error),
    };
    bindings.co_uninitialize();
    result
}

fn check(hr: i32, what: &str) -> anyhow::Result<i32> {
    if hr < 0 {
        anyhow::bail!("{what} failed: HRESULT 0x{:x}", hr.cast_unsigned())
    }
    Ok(hr)
}

/// Child-worker protocol.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Win32DialogWorkerMessage {
    /// The child is about to enter `Show`.
    Showing {
        /// Native dialog thread id.
        #[serde(rename = "threadId")]
        thread_id: u32,
    },
    /// Selection or user dismissal.
    Done {
        /// Selected path or cancellation.
        path: Option<String>,
    },
    /// Native worker failure.
    Error {
        /// Preserved worker diagnostic.
        message: String,
    },
}

/// Spawned worker event.
#[derive(Debug)]
pub enum WorkerEvent {
    /// Protocol message.
    Message(Win32DialogWorkerMessage),
    /// Child process error event.
    Error(anyhow::Error),
    /// Child exited before a terminal message.
    Exit,
}

/// Owned worker process surface.
pub struct Win32DialogWorker {
    /// Ordered worker events.
    pub events: mpsc::UnboundedReceiver<WorkerEvent>,
    /// Force termination fallback.
    pub kill: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Releases any runtime keepalive reference after settlement.
    pub unref: Arc<dyn Fn() + Send + Sync>,
}

impl fmt::Debug for Win32DialogWorker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Win32DialogWorker")
            .finish_non_exhaustive()
    }
}

/// Injectable worker/window/timer boundaries.
#[derive(Clone)]
pub struct Win32DialogInternals {
    /// Child worker spawn.
    pub spawn_worker: Arc<dyn Fn(String) -> anyhow::Result<Win32DialogWorker> + Send + Sync>,
    /// Posts `WM_CLOSE` to every dialog-thread window.
    pub close_thread_windows:
        Arc<dyn Fn(u32) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>,
    /// Retry delay seam.
    pub wait: Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>,
    /// Retry cadence.
    pub close_retry: Duration,
    /// Attempts before kill.
    pub close_max_attempts: usize,
}

impl fmt::Debug for Win32DialogInternals {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Win32DialogInternals")
            .field("close_retry", &self.close_retry)
            .field("close_max_attempts", &self.close_max_attempts)
            .finish_non_exhaustive()
    }
}

/// Opens the modern Win32 folder picker off the async executor.
///
/// # Errors
///
/// Returns worker/native failures or the stable caller-abort diagnostics.
pub async fn pick_win32_directory(
    signal: AbortSignal,
    internals: Option<&Win32DialogInternals>,
) -> anyhow::Result<Option<String>> {
    if signal.is_aborted() {
        anyhow::bail!("native directory picker aborted");
    }
    let owned;
    let internals = if let Some(internals) = internals {
        internals
    } else {
        owned = production_win32_dialog_internals(None);
        &owned
    };
    let mut worker = (internals.spawn_worker)(DIALOG_TITLE.to_owned())?;
    let mut dialog_thread_id = None;
    let mut aborting = false;
    let mut attempts = 0_usize;
    loop {
        tokio::select! {
            biased;
            () = signal.cancelled(), if !aborting => {
                aborting = true;
                post_close(internals, dialog_thread_id);
            }
            event = worker.events.recv() => {
                match event {
                    Some(WorkerEvent::Message(Win32DialogWorkerMessage::Showing { thread_id })) => {
                        dialog_thread_id = Some(thread_id);
                        if aborting || signal.is_aborted() {
                            aborting = true;
                            post_close(internals, dialog_thread_id);
                        }
                    }
                    Some(WorkerEvent::Message(Win32DialogWorkerMessage::Done { path })) => {
                        (worker.unref)();
                        if aborting || signal.is_aborted() {
                            anyhow::bail!("native directory picker aborted");
                        }
                        return Ok(path);
                    }
                    Some(WorkerEvent::Message(Win32DialogWorkerMessage::Error { message })) => {
                        (worker.unref)();
                        anyhow::bail!("win32 folder dialog failed: {message}");
                    }
                    Some(WorkerEvent::Error(error)) => {
                        (worker.unref)();
                        return Err(error);
                    }
                    Some(WorkerEvent::Exit) | None => {
                        (worker.unref)();
                        anyhow::bail!("win32 folder dialog worker exited before reporting a result");
                    }
                }
            }
            () = (internals.wait)(internals.close_retry), if aborting => {
                attempts = attempts.saturating_add(1);
                if attempts > internals.close_max_attempts {
                    (worker.unref)();
                    (worker.kill)();
                    anyhow::bail!("native directory picker aborted (dialog unresponsive; worker killed)");
                }
                post_close(internals, dialog_thread_id);
            }
        }
    }
}

fn post_close(internals: &Win32DialogInternals, thread_id: Option<u32>) {
    let Some(thread_id) = thread_id else { return };
    let close = (internals.close_thread_windows)(thread_id);
    tokio::spawn(async move {
        let _ = close.await;
    });
}

/// Builds the real worker-process and Win32 window-close boundaries.
///
/// `worker` overrides sibling-binary discovery for packaged launchers and
/// built-artifact tests.
#[must_use]
pub fn production_win32_dialog_internals(worker: Option<PathBuf>) -> Win32DialogInternals {
    Win32DialogInternals {
        spawn_worker: Arc::new(move |title| {
            let executable = worker.clone().map_or_else(worker_executable, Ok)?;
            spawn_dialog_worker(&executable, title)
        }),
        close_thread_windows: Arc::new(|thread_id| {
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    seekdeep_win32_directory_dialog::close_thread_windows(thread_id)
                })
                .await??;
                Ok(())
            })
        }),
        wait: Arc::new(|duration| Box::pin(tokio::time::sleep(duration))),
        close_retry: CLOSE_RETRY,
        close_max_attempts: CLOSE_MAX_ATTEMPTS,
    }
}

fn spawn_dialog_worker(
    executable: &std::path::Path,
    title: String,
) -> anyhow::Result<Win32DialogWorker> {
    let mut command = Command::new(executable);
    command
        .env("SEEKDEEP_DIALOG_TITLE", title)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    hide_worker_console(&mut command);
    let mut child = command.spawn().map_err(|error| {
        anyhow::anyhow!(
            "failed to spawn Win32 folder dialog worker {}: {error}",
            executable.display()
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Win32 folder dialog worker has no stdout pipe"))?;
    let child = Arc::new(parking_lot::Mutex::new(child));
    let (events, receiver) = mpsc::unbounded_channel();
    let terminal = Arc::new(AtomicBool::new(false));
    std::thread::spawn({
        let terminal = terminal.clone();
        move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        if !terminal.load(Ordering::Acquire) {
                            let _ = events.send(WorkerEvent::Exit);
                        }
                        break;
                    }
                    Ok(_) => match serde_json::from_str::<Win32DialogWorkerMessage>(&line) {
                        Ok(message) => {
                            if matches!(
                                message,
                                Win32DialogWorkerMessage::Done { .. }
                                    | Win32DialogWorkerMessage::Error { .. }
                            ) {
                                terminal.store(true, Ordering::Release);
                            }
                            if events.send(WorkerEvent::Message(message)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            terminal.store(true, Ordering::Release);
                            let _ = events.send(WorkerEvent::Error(anyhow::anyhow!(
                                "invalid Win32 folder dialog worker message: {error}"
                            )));
                            break;
                        }
                    },
                    Err(error) => {
                        terminal.store(true, Ordering::Release);
                        let _ = events.send(WorkerEvent::Error(anyhow::anyhow!(
                            "failed to read Win32 folder dialog worker: {error}"
                        )));
                        break;
                    }
                }
            }
        }
    });
    Ok(Win32DialogWorker {
        events: receiver,
        kill: Arc::new({
            let child = child.clone();
            move || child.lock().kill().is_ok()
        }),
        unref: Arc::new(|| {}),
    })
}

fn worker_executable() -> anyhow::Result<PathBuf> {
    if let Some(path) =
        std::env::var_os("SEEKDEEP_DIRECTORY_PICKER_WORKER").filter(|path| !path.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    let current = std::env::current_exe()?;
    let name = if cfg!(windows) {
        "seekdeep-directory-picker-worker.exe"
    } else {
        "seekdeep-directory-picker-worker"
    };
    Ok(current
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current executable has no parent directory"))?
        .join(name))
}

#[cfg(windows)]
fn hide_worker_console(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_worker_console(_command: &mut Command) {}
