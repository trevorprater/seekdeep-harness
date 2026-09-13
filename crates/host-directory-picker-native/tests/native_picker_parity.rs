//! Platform-tier, command, cancellation, and Cordis service parity.

use std::{collections::VecDeque, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_host_directory_picker::{DIRECTORY_PICKER, DirectoryPickerCapability};
use seekdeep_host_directory_picker_native::{
    DirectoryPickerInternals, HostPlatform, PickerCommandError, pick_native_directory,
    plugin_with_internals,
};
use seekdeep_llm::AbortSignal;
use seekdeep_util::native_command::{NativeCommandCode, NativeCommandOutput};

type Call = (String, Vec<String>);

fn output(stdout: &str) -> NativeCommandOutput {
    NativeCommandOutput {
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

fn failure(code: NativeCommandCode, stderr: &str) -> anyhow::Result<NativeCommandOutput> {
    let message = format!("command failed: {code}");
    Err(anyhow::Error::new(PickerCommandError::new(
        code,
        stderr,
        anyhow::anyhow!(message),
    )))
}

fn internals(
    platform: HostPlatform,
    outcomes: Vec<anyhow::Result<NativeCommandOutput>>,
) -> (DirectoryPickerInternals, Arc<Mutex<Vec<Call>>>) {
    let outcomes = Arc::new(Mutex::new(VecDeque::from(outcomes)));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let run = Arc::new({
        let outcomes = outcomes.clone();
        let calls = calls.clone();
        move |command: String, args: Vec<String>, _signal: AbortSignal| {
            calls.lock().push((command, args));
            let outcome = outcomes.lock().pop_front().expect("runner outcome");
            Box::pin(async move { outcome }) as futures::future::BoxFuture<'static, _>
        }
    });
    (
        DirectoryPickerInternals {
            platform,
            run,
            pick_win32_dialog: Arc::new(|_| Box::pin(async { Ok(None) })),
        },
        calls,
    )
}

#[tokio::test]
async fn macos_uses_osascript_and_only_maps_the_real_cancel_signature() {
    let (adapter, calls) = internals(
        HostPlatform::Darwin,
        vec![
            Ok(output("/Users/test/project/\n")),
            failure(
                NativeCommandCode::Exit(1),
                "execution error: User canceled. (-128)",
            ),
            failure(NativeCommandCode::Exit(2), "permission denied"),
        ],
    );
    assert_eq!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap(),
        Some("/Users/test/project/".to_owned())
    );
    assert_eq!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap(),
        None
    );
    assert!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap_err()
            .to_string()
            .contains("command failed")
    );
    {
        let calls = calls.lock();
        assert!(calls.iter().all(|(command, _)| command == "osascript"));
        assert!(
            calls[0]
                .1
                .iter()
                .any(|arg| arg == "POSIX path of selectedFolder")
        );
    }

    for (code, stderr) in [
        (NativeCommandCode::Named("UNKNOWN"), "User canceled. (-128)"),
        (NativeCommandCode::Exit(1), ""),
    ] {
        let (adapter, _) = internals(HostPlatform::Darwin, vec![failure(code, stderr)]);
        assert!(
            pick_native_directory(AbortSignal::default(), &adapter)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn windows_uses_only_the_dialog_and_preserves_its_selection_cancel_and_failure() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let run = Arc::new({
        let calls = calls.clone();
        move |command, args, _| {
            calls.lock().push((command, args));
            Box::pin(async { anyhow::bail!("must not run") })
                as futures::future::BoxFuture<'static, _>
        }
    });
    for expected in [Some("C:\\work\\selected".to_owned()), None] {
        let selected = expected.clone();
        let adapter = DirectoryPickerInternals {
            platform: HostPlatform::Win32,
            run: run.clone(),
            pick_win32_dialog: Arc::new(move |_| {
                let selected = selected.clone();
                Box::pin(async move { Ok(selected) })
            }),
        };
        assert_eq!(
            pick_native_directory(AbortSignal::default(), &adapter)
                .await
                .unwrap(),
            expected
        );
    }
    let adapter = DirectoryPickerInternals {
        platform: HostPlatform::Win32,
        run,
        pick_win32_dialog: Arc::new(|_| Box::pin(async { anyhow::bail!("dialog unavailable") })),
    };
    assert_eq!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap_err()
            .to_string(),
        "dialog unavailable"
    );
    assert!(calls.lock().is_empty());
}

#[tokio::test]
async fn linux_uses_zenity_then_only_missing_command_falls_back_to_kdialog() {
    let (adapter, calls) = internals(
        HostPlatform::Linux,
        vec![
            failure(NativeCommandCode::Named("ENOENT"), ""),
            Ok(output("/home/test/project\n")),
        ],
    );
    assert_eq!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap(),
        Some("/home/test/project".to_owned())
    );
    assert_eq!(
        calls
            .lock()
            .iter()
            .map(|(command, _)| command.as_str())
            .collect::<Vec<_>>(),
        ["zenity", "kdialog"]
    );

    for code in [NativeCommandCode::Exit(1)] {
        let (adapter, _) = internals(HostPlatform::Linux, vec![failure(code, "")]);
        assert_eq!(
            pick_native_directory(AbortSignal::default(), &adapter)
                .await
                .unwrap(),
            None
        );
    }
    let (adapter, _) = internals(
        HostPlatform::Linux,
        vec![
            failure(NativeCommandCode::Named("ENOENT"), ""),
            failure(NativeCommandCode::Exit(1), ""),
        ],
    );
    assert_eq!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap(),
        None
    );
    let (adapter, _) = internals(
        HostPlatform::Linux,
        vec![
            failure(NativeCommandCode::Named("ENOENT"), ""),
            failure(NativeCommandCode::Named("ENOENT"), ""),
        ],
    );
    assert!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap_err()
            .to_string()
            .contains("install zenity or kdialog")
    );
    for outcomes in [
        vec![failure(NativeCommandCode::Exit(2), "")],
        vec![
            failure(NativeCommandCode::Named("ENOENT"), ""),
            failure(NativeCommandCode::Exit(2), ""),
        ],
    ] {
        let (adapter, _) = internals(HostPlatform::Linux, outcomes);
        assert!(
            pick_native_directory(AbortSignal::default(), &adapter)
                .await
                .unwrap_err()
                .to_string()
                .contains("command failed")
        );
    }
}

#[tokio::test]
async fn empty_output_abort_current_platform_and_unsupported_platform_keep_source_rules() {
    let (adapter, _) = internals(HostPlatform::Linux, vec![Ok(output(""))]);
    assert_eq!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap(),
        None
    );

    let aborted = AbortSignal::default();
    aborted.abort_with_reason(serde_json::json!("closed"));
    let (adapter, _) = internals(
        HostPlatform::Linux,
        vec![failure(NativeCommandCode::Named("ABORT_ERR"), "")],
    );
    assert!(pick_native_directory(aborted, &adapter).await.is_err());

    let (adapter, _) = internals(HostPlatform::Other("aix".to_owned()), Vec::new());
    assert_eq!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap_err()
            .to_string(),
        "native directory picker is unsupported on aix"
    );

    let expected = match HostPlatform::current() {
        HostPlatform::Win32 => Some("C:\\default\\platform".to_owned()),
        HostPlatform::Darwin | HostPlatform::Linux => Some("/default/platform".to_owned()),
        HostPlatform::Other(_) => return,
    };
    let (mut adapter, _) = internals(
        HostPlatform::current(),
        vec![Ok(output("/default/platform\n"))],
    );
    adapter.pick_win32_dialog =
        Arc::new(|_| Box::pin(async { Ok(Some("C:\\default\\platform".to_owned())) }));
    assert_eq!(
        pick_native_directory(AbortSignal::default(), &adapter)
            .await
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn cordis_plugin_exposes_one_stable_capability_for_its_fiber_lifetime() {
    let (adapter, _) = internals(HostPlatform::Linux, vec![Ok(output("/chosen\n"))]);
    let context = Context::new();
    let fiber = context
        .plugin(plugin_with_internals(adapter), serde_json::Value::Null)
        .unwrap();
    fiber.await_settled().await.unwrap();
    let picker = context.get(DIRECTORY_PICKER).unwrap();
    let first = picker.capability() as *const _;
    let second = picker.capability() as *const _;
    assert_eq!(first, second);
    let DirectoryPickerCapability::Native { pick } = picker.capability() else {
        panic!("expected native capability")
    };
    assert_eq!(
        pick(AbortSignal::default()).await.unwrap(),
        Some("/chosen".to_owned())
    );
    fiber.dispose().await.unwrap();
    assert!(context.get(DIRECTORY_PICKER).is_none());
}
