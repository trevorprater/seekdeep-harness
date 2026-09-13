//! Win32 COM sequencing and abortable child-driver parity on every host.

use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
#[cfg(not(windows))]
use seekdeep_host_directory_picker_native::win32_dialog::production_win32_dialog_internals;
use seekdeep_host_directory_picker_native::win32_dialog::{
    CLOSE_MAX_ATTEMPTS, DIALOG_TITLE, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PICKFOLDERS,
    HRESULT_CANCELLED, Win32DialogBindings, Win32DialogInternals, Win32DialogWorker,
    Win32DialogWorkerMessage, Win32FolderDialog, WorkerEvent, pick_win32_directory,
    run_folder_dialog,
};
use seekdeep_llm::AbortSignal;
use tokio::sync::mpsc;

const E_FAIL: i32 = 0x8000_4005_u32.cast_signed();

#[derive(Debug, Default)]
struct ComState {
    calls: Vec<String>,
    options: Vec<u32>,
    titles: Vec<String>,
    init: i32,
    set_options: i32,
    set_title: i32,
    show: i32,
    result: i32,
    path: Option<String>,
    create_error: Option<String>,
}

struct FakeDialog(Arc<Mutex<ComState>>);

impl Win32FolderDialog for FakeDialog {
    fn set_options(&mut self, options: u32) -> i32 {
        let mut state = self.0.lock();
        state.calls.push("options".to_owned());
        state.options.push(options);
        state.set_options
    }

    fn set_title(&mut self, title: &str) -> i32 {
        let mut state = self.0.lock();
        state.calls.push("title".to_owned());
        state.titles.push(title.to_owned());
        state.set_title
    }

    fn show(&mut self) -> i32 {
        let mut state = self.0.lock();
        state.calls.push("show".to_owned());
        state.show
    }

    fn result_path(&mut self) -> (i32, Option<String>) {
        let mut state = self.0.lock();
        state.calls.push("result".to_owned());
        (state.result, state.path.clone())
    }

    fn release(&mut self) {
        self.0.lock().calls.push("release".to_owned());
    }
}

struct FakeBindings(Arc<Mutex<ComState>>);

impl Win32DialogBindings for FakeBindings {
    fn set_thread_dpi_awareness(&mut self) {
        self.0.lock().calls.push("dpi".to_owned());
    }

    fn co_initialize_sta(&mut self) -> i32 {
        let mut state = self.0.lock();
        state.calls.push("init".to_owned());
        state.init
    }

    fn co_uninitialize(&mut self) {
        self.0.lock().calls.push("uninit".to_owned());
    }

    fn create_folder_dialog(&mut self) -> anyhow::Result<Box<dyn Win32FolderDialog>> {
        let mut state = self.0.lock();
        state.calls.push("create".to_owned());
        if let Some(error) = &state.create_error {
            anyhow::bail!(error.clone());
        }
        drop(state);
        Ok(Box::new(FakeDialog(self.0.clone())))
    }

    fn current_thread_id(&self) -> u32 {
        self.0.lock().calls.push("thread".to_owned());
        31_337
    }
}

fn fake_bindings(state: ComState) -> (FakeBindings, Arc<Mutex<ComState>>) {
    let state = Arc::new(Mutex::new(state));
    (FakeBindings(state.clone()), state)
}

#[test]
fn folder_dialog_sequences_every_step_and_cleans_success_and_cancellation() {
    let (mut bindings, state) = fake_bindings(ComState {
        path: Some("C:\\选中\\directory".to_owned()),
        ..ComState::default()
    });
    let showing = Arc::new(Mutex::new(Vec::new()));
    let path = run_folder_dialog(&mut bindings, "选择工作区目录", {
        let showing = showing.clone();
        move |thread| showing.lock().push(thread)
    })
    .unwrap();
    assert_eq!(path.as_deref(), Some("C:\\选中\\directory"));
    assert_eq!(*showing.lock(), [31_337]);
    let state = state.lock();
    assert_eq!(
        state.calls,
        [
            "dpi", "init", "create", "options", "title", "thread", "show", "result", "release",
            "uninit",
        ]
    );
    assert_eq!(
        state.options,
        [FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR]
    );
    assert_eq!(state.titles, ["选择工作区目录"]);
    drop(state);

    let (mut bindings, state) = fake_bindings(ComState {
        init: 1,
        show: HRESULT_CANCELLED,
        ..ComState::default()
    });
    assert_eq!(
        run_folder_dialog(&mut bindings, "Pick", |_| {}).unwrap(),
        None
    );
    assert_eq!(
        state.lock().calls.last().map(String::as_str),
        Some("uninit")
    );
    assert!(state.lock().calls.contains(&"release".to_owned()));
}

#[test]
fn folder_dialog_surfaces_each_hresult_and_preserves_cleanup_boundaries() {
    let (mut bindings, state) = fake_bindings(ComState {
        init: E_FAIL,
        ..ComState::default()
    });
    assert!(
        run_folder_dialog(&mut bindings, "Pick", |_| {})
            .unwrap_err()
            .to_string()
            .contains("CoInitializeEx failed: HRESULT 0x80004005")
    );
    assert_eq!(state.lock().calls, ["dpi", "init"]);

    for (field, expected) in [
        ("options", "SetOptions failed"),
        ("title", "SetTitle failed"),
        ("show", "Show failed"),
        ("result", "GetResult failed"),
    ] {
        let mut state = ComState {
            path: Some("C:\\selected".to_owned()),
            ..ComState::default()
        };
        match field {
            "options" => state.set_options = E_FAIL,
            "title" => state.set_title = E_FAIL,
            "show" => state.show = E_FAIL,
            "result" => state.result = E_FAIL,
            _ => unreachable!(),
        }
        let (mut bindings, state) = fake_bindings(state);
        assert!(
            run_folder_dialog(&mut bindings, "Pick", |_| {})
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
        let calls = &state.lock().calls;
        assert!(calls.contains(&"release".to_owned()));
        assert_eq!(calls.last().map(String::as_str), Some("uninit"));
    }

    let (mut bindings, state) = fake_bindings(ComState {
        create_error: Some("CoCreateInstance(FileOpenDialog) failed".to_owned()),
        ..ComState::default()
    });
    assert!(
        run_folder_dialog(&mut bindings, "Pick", |_| {})
            .unwrap_err()
            .to_string()
            .contains("CoCreateInstance")
    );
    assert_eq!(
        state.lock().calls.last().map(String::as_str),
        Some("uninit")
    );
}

struct DriverHarness {
    internals: Win32DialogInternals,
    events: mpsc::UnboundedSender<WorkerEvent>,
    spawn_count: Arc<Mutex<usize>>,
    closes: Arc<Mutex<Vec<u32>>>,
    killed: Arc<Mutex<usize>>,
    unrefed: Arc<Mutex<usize>>,
}

fn driver(max_attempts: usize) -> DriverHarness {
    let (events, receiver) = mpsc::unbounded_channel();
    let receiver = Arc::new(Mutex::new(Some(receiver)));
    let spawn_count = Arc::new(Mutex::new(0));
    let closes = Arc::new(Mutex::new(Vec::new()));
    let killed = Arc::new(Mutex::new(0));
    let unrefed = Arc::new(Mutex::new(0));
    let internals = Win32DialogInternals {
        spawn_worker: Arc::new({
            let receiver = receiver.clone();
            let spawn_count = spawn_count.clone();
            let killed = killed.clone();
            let unrefed = unrefed.clone();
            move |title| {
                assert_eq!(title, DIALOG_TITLE);
                *spawn_count.lock() += 1;
                Ok(Win32DialogWorker {
                    events: receiver.lock().take().expect("spawn once"),
                    kill: Arc::new({
                        let killed = killed.clone();
                        move || {
                            *killed.lock() += 1;
                            true
                        }
                    }),
                    unref: Arc::new({
                        let unrefed = unrefed.clone();
                        move || *unrefed.lock() += 1
                    }),
                })
            }
        }),
        close_thread_windows: Arc::new({
            let closes = closes.clone();
            move |thread| {
                closes.lock().push(thread);
                Box::pin(async { Ok(()) })
            }
        }),
        wait: Arc::new(|_| Box::pin(async { tokio::task::yield_now().await })),
        close_retry: Duration::ZERO,
        close_max_attempts: max_attempts,
    };
    DriverHarness {
        internals,
        events,
        spawn_count,
        closes,
        killed,
        unrefed,
    }
}

#[tokio::test]
async fn driver_resolves_terminal_messages_and_ignores_late_events() {
    for expected in [Some("C:\\selected".to_owned()), None] {
        let harness = driver(CLOSE_MAX_ATTEMPTS);
        harness
            .events
            .send(WorkerEvent::Message(Win32DialogWorkerMessage::Done {
                path: expected.clone(),
            }))
            .unwrap();
        assert_eq!(
            pick_win32_directory(AbortSignal::default(), Some(&harness.internals))
                .await
                .unwrap(),
            expected
        );
        assert_eq!(*harness.unrefed.lock(), 1);
        let _ = harness.events.send(WorkerEvent::Exit);
        assert_eq!(*harness.unrefed.lock(), 1);
    }

    let harness = driver(CLOSE_MAX_ATTEMPTS);
    harness
        .events
        .send(WorkerEvent::Message(Win32DialogWorkerMessage::Error {
            message: "COM refused".to_owned(),
        }))
        .unwrap();
    assert_eq!(
        pick_win32_directory(AbortSignal::default(), Some(&harness.internals))
            .await
            .unwrap_err()
            .to_string(),
        "win32 folder dialog failed: COM refused"
    );

    for event in [
        WorkerEvent::Error(anyhow::anyhow!("spawned worker crash")),
        WorkerEvent::Exit,
    ] {
        let harness = driver(CLOSE_MAX_ATTEMPTS);
        harness.events.send(event).unwrap();
        assert!(
            pick_win32_directory(AbortSignal::default(), Some(&harness.internals))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn preaborted_signal_never_spawns_and_live_abort_closes_until_done() {
    let harness = driver(CLOSE_MAX_ATTEMPTS);
    let aborted = AbortSignal::default();
    aborted.abort();
    assert_eq!(
        pick_win32_directory(aborted, Some(&harness.internals))
            .await
            .unwrap_err()
            .to_string(),
        "native directory picker aborted"
    );
    assert_eq!(*harness.spawn_count.lock(), 0);

    let harness = driver(CLOSE_MAX_ATTEMPTS);
    let signal = AbortSignal::default();
    let task = tokio::spawn({
        let signal = signal.clone();
        let internals = harness.internals.clone();
        async move { pick_win32_directory(signal, Some(&internals)).await }
    });
    while *harness.spawn_count.lock() == 0 {
        tokio::task::yield_now().await;
    }
    harness
        .events
        .send(WorkerEvent::Message(Win32DialogWorkerMessage::Showing {
            thread_id: 99,
        }))
        .unwrap();
    signal.abort();
    while harness.closes.lock().is_empty() {
        tokio::task::yield_now().await;
    }
    harness
        .events
        .send(WorkerEvent::Message(Win32DialogWorkerMessage::Done {
            path: None,
        }))
        .unwrap();
    assert_eq!(
        task.await.unwrap().unwrap_err().to_string(),
        "native directory picker aborted"
    );
    assert!(harness.closes.lock().iter().all(|thread| *thread == 99));
    assert_eq!(*harness.killed.lock(), 0);
    assert_eq!(*harness.unrefed.lock(), 1);
}

#[tokio::test]
async fn abort_before_showing_retries_after_notice_and_kills_each_unresponsive_worker() {
    let harness = driver(2);
    let signal = AbortSignal::default();
    let task = tokio::spawn({
        let signal = signal.clone();
        let internals = harness.internals.clone();
        async move { pick_win32_directory(signal, Some(&internals)).await }
    });
    while *harness.spawn_count.lock() == 0 {
        tokio::task::yield_now().await;
    }
    signal.abort();
    tokio::task::yield_now().await;
    assert!(harness.closes.lock().is_empty());
    harness
        .events
        .send(WorkerEvent::Message(Win32DialogWorkerMessage::Showing {
            thread_id: 12,
        }))
        .unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("dialog unresponsive; worker killed")
    );
    assert!(!harness.closes.lock().is_empty());
    assert_eq!(*harness.killed.lock(), 1);

    let harness = driver(1);
    let signal = AbortSignal::default();
    let task = tokio::spawn({
        let signal = signal.clone();
        let internals = harness.internals.clone();
        async move { pick_win32_directory(signal, Some(&internals)).await }
    });
    while *harness.spawn_count.lock() == 0 {
        tokio::task::yield_now().await;
    }
    signal.abort();
    assert!(
        task.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("dialog unresponsive; worker killed")
    );
    assert!(harness.closes.lock().is_empty());
    assert_eq!(*harness.killed.lock(), 1);
}

#[cfg(not(windows))]
#[tokio::test]
async fn built_worker_reports_the_real_non_windows_native_surface_failure() {
    let worker = std::path::PathBuf::from(env!("CARGO_BIN_EXE_seekdeep-directory-picker-worker"));
    let internals = production_win32_dialog_internals(Some(worker));
    let error = pick_win32_directory(AbortSignal::default(), Some(&internals))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("win32 folder dialog failed: Win32 folder dialog is unavailable")
    );
}
