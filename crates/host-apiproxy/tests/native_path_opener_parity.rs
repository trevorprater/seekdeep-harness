//! Behavioral mirror of `native-path-opener.spec.ts`.

use std::{collections::HashMap, sync::Arc};

use futures::FutureExt as _;
use parking_lot::Mutex;
use seekdeep_host_apiproxy::{
    NativePlatform, PathOpenerInternals, PathOpenerRunner, can_open_native_path, open_native_path,
    open_native_text_file,
};
use seekdeep_llm::AbortSignal;
use seekdeep_util::native_command::NativeCommandOutput;
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Call {
    command: String,
    args: Vec<String>,
}

type Calls = Arc<Mutex<Vec<Call>>>;

fn scripted_runner(
    callback: impl Fn(&str, &[String], &AbortSignal) -> anyhow::Result<NativeCommandOutput>
    + Send
    + Sync
    + 'static,
) -> (PathOpenerRunner, Calls) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let callback = Arc::new(callback);
    let runner: PathOpenerRunner = Arc::new({
        let calls = calls.clone();
        move |command, args, signal| {
            calls.lock().push(Call {
                command: command.clone(),
                args: args.clone(),
            });
            let callback = callback.clone();
            async move { callback(&command, &args, &signal) }.boxed()
        }
    });
    (runner, calls)
}

fn ok() -> NativeCommandOutput {
    NativeCommandOutput {
        stdout: String::new(),
        stderr: String::new(),
    }
}

fn facts(platform: NativePlatform, run: PathOpenerRunner) -> PathOpenerInternals {
    PathOpenerInternals {
        platform: Some(platform),
        os_release: Some("6.8.0-generic".to_owned()),
        env: Some(HashMap::new()),
        run: Some(run),
    }
}

#[tokio::test]
async fn macos_default_and_text_editor_intents_use_exact_open_arguments() {
    let (runner, calls) = scripted_runner(|_, _, _| Ok(ok()));
    let internals = facts(NativePlatform::Darwin, runner);
    open_native_path("/Users/test/file.txt", &AbortSignal::default(), &internals)
        .await
        .unwrap();
    open_native_text_file(
        "/Users/test/settings.yaml",
        &AbortSignal::default(),
        &internals,
    )
    .await
    .unwrap();
    assert_eq!(
        *calls.lock(),
        vec![
            Call {
                command: "open".to_owned(),
                args: vec!["/Users/test/file.txt".to_owned()],
            },
            Call {
                command: "open".to_owned(),
                args: vec!["-t".to_owned(), "/Users/test/settings.yaml".to_owned()],
            }
        ]
    );
}

#[tokio::test]
async fn desktop_linux_uses_xdg_open_for_default_and_text_intents() {
    let (runner, calls) = scripted_runner(|_, _, _| Ok(ok()));
    let internals = facts(NativePlatform::Linux, runner);
    open_native_path("/tmp/a.txt", &AbortSignal::default(), &internals)
        .await
        .unwrap();
    open_native_text_file("/tmp/settings.yaml", &AbortSignal::default(), &internals)
        .await
        .unwrap();
    assert_eq!(calls.lock()[0].command, "xdg-open");
    assert_eq!(calls.lock()[1].command, "xdg-open");
}

#[tokio::test]
async fn every_wsl_marker_translates_then_invokes_windows_without_a_shell() {
    let cases = [
        (
            HashMap::from([("WSL_DISTRO_NAME".to_owned(), "Ubuntu".to_owned())]),
            "6.8.0-generic",
        ),
        (
            HashMap::from([("WSL_INTEROP".to_owned(), "/run/WSL/123_interop".to_owned())]),
            "6.8.0-generic",
        ),
        (HashMap::new(), "5.15.153.1-microsoft-standard-WSL2"),
    ];
    for (env, release) in cases {
        let (runner, calls) = scripted_runner(|command, _, _| {
            Ok(NativeCommandOutput {
                stdout: if command == "wslpath" {
                    "\\\\wsl.localhost\\Ubuntu\\home\\test user\\settings.yaml\r\n".to_owned()
                } else {
                    String::new()
                },
                stderr: String::new(),
            })
        });
        let internals = PathOpenerInternals {
            platform: Some(NativePlatform::Linux),
            os_release: Some(release.to_owned()),
            env: Some(env),
            run: Some(runner),
        };
        open_native_text_file(
            "/home/test user/settings.yaml",
            &AbortSignal::default(),
            &internals,
        )
        .await
        .unwrap();
        assert_eq!(
            *calls.lock(),
            vec![
                Call {
                    command: "wslpath".to_owned(),
                    args: vec!["-w".to_owned(), "/home/test user/settings.yaml".to_owned()],
                },
                Call {
                    command: "powershell.exe".to_owned(),
                    args: vec![
                        "-NoProfile".to_owned(),
                        "-Command".to_owned(),
                        "Invoke-Item -LiteralPath '\\\\wsl.localhost\\Ubuntu\\home\\test user\\settings.yaml'".to_owned(),
                    ],
                },
            ]
        );
    }
}

#[tokio::test]
async fn wsl_rejects_empty_translation_and_rechecks_abort_before_windows() {
    let (empty_runner, empty_calls) = scripted_runner(|_, _, _| {
        Ok(NativeCommandOutput {
            stdout: "\r\n".to_owned(),
            stderr: String::new(),
        })
    });
    let mut internals = facts(NativePlatform::Linux, empty_runner);
    internals.env = Some(HashMap::from([(
        "WSL_DISTRO_NAME".to_owned(),
        "Ubuntu".to_owned(),
    )]));
    let error = open_native_text_file(
        "/home/test/settings.yaml",
        &AbortSignal::default(),
        &internals,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("wslpath returned no Windows path")
    );
    assert_eq!(empty_calls.lock().len(), 1);

    let signal = AbortSignal::default();
    let aborting = signal.clone();
    let (abort_runner, abort_calls) = scripted_runner(move |_, _, _| {
        aborting.abort_with_reason(json!("closed"));
        Ok(NativeCommandOutput {
            stdout: "C:\\settings.yaml\n".to_owned(),
            stderr: String::new(),
        })
    });
    internals.run = Some(abort_runner);
    let error = open_native_text_file("/home/test/settings.yaml", &signal, &internals)
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "closed");
    assert_eq!(abort_calls.lock().len(), 1);
}

#[tokio::test]
async fn windows_uses_invoke_item_and_escapes_single_quotes_for_both_intents() {
    let (runner, calls) = scripted_runner(|_, _, _| Ok(ok()));
    let internals = facts(NativePlatform::Windows, runner);
    open_native_path(
        "C:\\work\\o'reilly.txt",
        &AbortSignal::default(),
        &internals,
    )
    .await
    .unwrap();
    open_native_text_file(
        "C:\\work\\settings.yaml",
        &AbortSignal::default(),
        &internals,
    )
    .await
    .unwrap();
    assert_eq!(
        calls.lock()[0].args[2],
        "Invoke-Item -LiteralPath 'C:\\work\\o''reilly.txt'"
    );
    assert_eq!(
        calls.lock()[1].args[2],
        "Invoke-Item -LiteralPath 'C:\\work\\settings.yaml'"
    );
}

#[tokio::test]
async fn unsupported_platform_fails_with_the_source_platform_name() {
    let (runner, calls) = scripted_runner(|_, _, _| Ok(ok()));
    let internals = facts(NativePlatform::Other("freebsd".to_owned()), runner);
    let error = open_native_path("/x", &AbortSignal::default(), &internals)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unsupported on freebsd"));
    assert!(calls.lock().is_empty());
}

const LS_PLIST: &str = r#"{
    LSHandlers = (
        {
            LSHandlerPreferredVersions = {
                LSHandlerRoleAll = "-";
            };
            LSHandlerRoleAll = "com.google.chrome";
            LSHandlerURLScheme = https;
        }
    );
}"#;

#[tokio::test]
async fn macos_browser_documents_resolve_https_bundle_and_other_extensions_do_not_probe() {
    let (runner, calls) = scripted_runner(|command, _, _| {
        Ok(NativeCommandOutput {
            stdout: if command == "defaults" {
                LS_PLIST.to_owned()
            } else {
                String::new()
            },
            stderr: String::new(),
        })
    });
    let internals = facts(NativePlatform::Darwin, runner);
    open_native_path("/w/page.HTML", &AbortSignal::default(), &internals)
        .await
        .unwrap();
    assert_eq!(calls.lock()[0].command, "defaults");
    assert_eq!(
        calls.lock()[1].args,
        vec![
            "-b".to_owned(),
            "com.google.chrome".to_owned(),
            "/w/page.HTML".to_owned()
        ]
    );
    calls.lock().clear();
    open_native_path("/w/report.md", &AbortSignal::default(), &internals)
        .await
        .unwrap();
    assert_eq!(
        *calls.lock(),
        vec![Call {
            command: "open".to_owned(),
            args: vec!["/w/report.md".to_owned()]
        }]
    );
}

#[tokio::test]
async fn macos_browser_lookup_failure_or_missing_record_falls_back_to_association() {
    let (failing_runner, failing_calls) = scripted_runner(|command, _, _| {
        if command == "defaults" {
            anyhow::bail!("domain not found");
        }
        Ok(ok())
    });
    let internals = facts(NativePlatform::Darwin, failing_runner);
    open_native_path("/w/page.html", &AbortSignal::default(), &internals)
        .await
        .unwrap();
    assert_eq!(failing_calls.lock()[1].command, "open");
    assert_eq!(failing_calls.lock()[1].args, vec!["/w/page.html"]);

    let (bare_runner, bare_calls) = scripted_runner(|_, _, _| {
        Ok(NativeCommandOutput {
            stdout: "{ LSHandlers = ( ); }".to_owned(),
            stderr: String::new(),
        })
    });
    let internals = facts(NativePlatform::Darwin, bare_runner);
    open_native_path("/w/page.html", &AbortSignal::default(), &internals)
        .await
        .unwrap();
    assert_eq!(bare_calls.lock()[1].command, "open");
}

#[tokio::test]
async fn linux_honors_browser_but_wsl_always_hands_renderable_paths_to_windows() {
    let (browser_runner, browser_calls) = scripted_runner(|_, _, _| Ok(ok()));
    let mut linux = facts(NativePlatform::Linux, browser_runner);
    linux.env = Some(HashMap::from([(
        "BROWSER".to_owned(),
        "firefox".to_owned(),
    )]));
    open_native_path("/w/page.svg", &AbortSignal::default(), &linux)
        .await
        .unwrap();
    assert_eq!(browser_calls.lock()[0].command, "firefox");

    let (wsl_runner, wsl_calls) = scripted_runner(|command, _, _| {
        Ok(NativeCommandOutput {
            stdout: if command == "wslpath" {
                "C:\\workspace\\page.html\n".to_owned()
            } else {
                String::new()
            },
            stderr: String::new(),
        })
    });
    linux.run = Some(wsl_runner);
    linux.os_release = Some("5.15.153.1-microsoft-standard-WSL2".to_owned());
    open_native_path("/home/test/page.html", &AbortSignal::default(), &linux)
        .await
        .unwrap();
    assert_eq!(wsl_calls.lock()[0].command, "wslpath");
    assert_eq!(wsl_calls.lock()[1].command, "powershell.exe");
}

#[test]
fn capability_matches_desktop_wsl_display_and_unsupported_platform_facts() {
    assert!(can_open_native_path(&PathOpenerInternals {
        platform: Some(NativePlatform::Darwin),
        env: Some(HashMap::new()),
        ..PathOpenerInternals::default()
    }));
    assert!(can_open_native_path(&PathOpenerInternals {
        platform: Some(NativePlatform::Windows),
        env: Some(HashMap::new()),
        ..PathOpenerInternals::default()
    }));
    let linux = |env, release: &str| PathOpenerInternals {
        platform: Some(NativePlatform::Linux),
        os_release: Some(release.to_owned()),
        env: Some(env),
        run: None,
    };
    assert!(!can_open_native_path(&linux(
        HashMap::new(),
        "6.8.0-generic"
    )));
    assert!(can_open_native_path(&linux(
        HashMap::from([("DISPLAY".to_owned(), ":0".to_owned())]),
        "6.8.0-generic"
    )));
    assert!(can_open_native_path(&linux(
        HashMap::from([("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned())]),
        "6.8.0-generic"
    )));
    assert!(can_open_native_path(&linux(
        HashMap::new(),
        "5.15-microsoft-standard"
    )));
    assert!(!can_open_native_path(&PathOpenerInternals {
        platform: Some(NativePlatform::Other("freebsd".to_owned())),
        env: Some(HashMap::new()),
        ..PathOpenerInternals::default()
    }));
}
