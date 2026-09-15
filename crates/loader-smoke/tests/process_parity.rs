//! Rust-native example selection and isolated subprocess parity.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use parking_lot::Mutex;
use seekdeep_loader_smoke::{
    ExampleLaunch, ExampleLaunchOptions, ExampleMode, LoaderSmokeOptions, run_loader_smoke,
};
use serde_json::json;

fn launch(executable: &str, args: &[&str]) -> ExampleLaunch {
    ExampleLaunch {
        command: PathBuf::from(executable),
        args: args.iter().map(OsString::from).collect(),
        environment: BTreeMap::new(),
    }
}

fn canonical_temporary_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref().to_string_lossy();
    PathBuf::from(path.strip_prefix("/private").unwrap_or(&path))
}

#[test]
fn mode_parser_and_compiled_artifact_selection_fail_loud() {
    for raw in [None, Some(""), Some("src")] {
        assert_eq!(ExampleMode::resolve(raw).unwrap(), ExampleMode::Source);
    }
    assert_eq!(
        ExampleMode::resolve(Some("lib")).unwrap(),
        ExampleMode::Library
    );
    assert!(
        ExampleMode::resolve(Some("prod"))
            .unwrap_err()
            .to_string()
            .contains("must be 'src' or 'lib'")
    );

    let source = ExampleLaunchOptions {
        source_bin: PathBuf::from("/repo/target/debug/seekdeep-acp-demo"),
        library_bin: Some(PathBuf::from("/repo/target/release/seekdeep-acp-demo")),
        config_args: vec![OsString::from("--config"), OsString::from("./cordis.yml")],
        mode: Some(ExampleMode::Source),
        environment: BTreeMap::from([(
            OsString::from("SEEKDEEP_HOME"),
            OsString::from("/tmp/home"),
        )]),
    }
    .resolve()
    .unwrap();
    assert_eq!(
        source.command,
        Path::new("/repo/target/debug/seekdeep-acp-demo")
    );
    assert_eq!(source.args, ["--config", "./cordis.yml"]);
    assert_eq!(
        source
            .environment
            .get(std::ffi::OsStr::new("SEEKDEEP_HOME")),
        Some(&OsString::from("/tmp/home"))
    );

    let library = ExampleLaunchOptions {
        source_bin: PathBuf::from("/repo/target/debug/seekdeep-acp-demo"),
        library_bin: Some(PathBuf::from("/repo/dist/seekdeep-acp-demo")),
        config_args: vec![OsString::from("./cordis.yml")],
        mode: Some(ExampleMode::Library),
        environment: BTreeMap::new(),
    }
    .resolve()
    .unwrap();
    assert_eq!(library.command, Path::new("/repo/dist/seekdeep-acp-demo"));
    assert_eq!(library.args, ["./cordis.yml"]);

    let missing = ExampleLaunchOptions {
        source_bin: PathBuf::from("/repo/target/debug/seekdeep-acp-demo"),
        library_bin: None,
        config_args: Vec::new(),
        mode: Some(ExampleMode::Library),
        environment: BTreeMap::new(),
    }
    .resolve()
    .unwrap_err();
    assert!(missing.to_string().contains("needs libraryBin"));
}

#[tokio::test]
async fn isolates_closes_stdin_captures_output_and_removes_working_directory() {
    let mut launch = launch(
        env!("CARGO_BIN_EXE_loader-smoke-success"),
        &["/tmp/fixture.cordis.yml"],
    );
    launch.environment.insert(
        OsString::from("LOADER_SMOKE_MARKER"),
        OsString::from("present"),
    );
    let result = run_loader_smoke(LoaderSmokeOptions::new(
        "success fixture",
        "loader-smoke-success-",
        launch,
    ))
    .await
    .unwrap();
    let output: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
    assert_eq!(
        output,
        json!({
            "configPath":"/tmp/fixture.cordis.yml",
            "args":["/tmp/fixture.cordis.yml"],
            "cwd":output["cwd"],
            "seekdeepHome":output["seekdeepHome"],
            "agentsHome":output["agentsHome"],
            "marker":"present",
            "input":""
        })
    );
    let cwd = output["cwd"].as_str().unwrap();
    assert_eq!(
        canonical_temporary_path(output["seekdeepHome"].as_str().unwrap()),
        canonical_temporary_path(Path::new(cwd).join(".seekdeep"))
    );
    assert_eq!(
        canonical_temporary_path(output["agentsHome"].as_str().unwrap()),
        canonical_temporary_path(Path::new(cwd).join(".agents"))
    );
    assert_eq!(result.stderr, "fixture stderr\n");
    assert!(!Path::new(cwd).exists());
}

#[tokio::test]
async fn arbitrary_argv_setup_and_inspection_run_before_cleanup() {
    let inspected = Arc::new(Mutex::new(None::<PathBuf>));
    let marker = Arc::new(Mutex::new(String::new()));
    let mut options = LoaderSmokeOptions::new(
        "argv fixture",
        "loader-smoke-argv-",
        launch(
            env!("CARGO_BIN_EXE_loader-smoke-success"),
            &[
                "--config",
                "/tmp/fixture.cordis.yml",
                "--output-format",
                "json",
                "task with spaces",
            ],
        ),
    );
    options.prepare = Some(Arc::new(|cwd| {
        Box::pin(async move {
            tokio::fs::write(cwd.join("marker.txt"), "prepared").await?;
            Ok(())
        })
    }));
    let inspected_sink = Arc::clone(&inspected);
    let marker_sink = Arc::clone(&marker);
    options.inspect = Some(Arc::new(move |cwd| {
        let inspected = Arc::clone(&inspected_sink);
        let marker = Arc::clone(&marker_sink);
        Box::pin(async move {
            *marker.lock() = tokio::fs::read_to_string(cwd.join("marker.txt")).await?;
            *inspected.lock() = Some(cwd);
            Ok(())
        })
    }));
    let result = run_loader_smoke(options).await.unwrap();
    let output: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
    assert_eq!(
        output["args"],
        json!([
            "--config",
            "/tmp/fixture.cordis.yml",
            "--output-format",
            "json",
            "task with spaces"
        ])
    );
    assert_eq!(&*marker.lock(), "prepared");
    let inspected = inspected.lock().clone().unwrap();
    assert_eq!(
        canonical_temporary_path(&inspected),
        canonical_temporary_path(output["cwd"].as_str().unwrap())
    );
    assert!(!inspected.exists());
}

#[tokio::test]
async fn exact_exit_contract_and_timeout_include_captured_diagnostics() {
    let failed = run_loader_smoke(LoaderSmokeOptions::new(
        "failure fixture",
        "loader-smoke-fail-",
        launch(env!("CARGO_BIN_EXE_loader-smoke-fail"), &[]),
    ))
    .await
    .unwrap_err();
    let failure = failed.to_string();
    assert!(failure.contains("failure fixture exited 7 (expected 0)"));
    assert!(failure.contains("stderr:\nfixture failed\n"));

    let mut declared = LoaderSmokeOptions::new(
        "declared failure fixture",
        "loader-smoke-declared-",
        launch(env!("CARGO_BIN_EXE_loader-smoke-fail"), &[]),
    );
    declared.expected_exit_code = 7;
    assert_eq!(
        run_loader_smoke(declared).await.unwrap().stderr,
        "fixture failed\n"
    );

    let mut unexpectedly_clean = LoaderSmokeOptions::new(
        "unexpectedly clean fixture",
        "loader-smoke-clean-",
        launch(env!("CARGO_BIN_EXE_loader-smoke-success"), &[]),
    );
    unexpectedly_clean.expected_exit_code = 7;
    assert!(
        run_loader_smoke(unexpectedly_clean)
            .await
            .unwrap_err()
            .to_string()
            .contains("exited 0 (expected 7)")
    );

    let mut hanging = LoaderSmokeOptions::new(
        "hanging fixture",
        "loader-smoke-hang-",
        launch(env!("CARGO_BIN_EXE_loader-smoke-hang"), &[]),
    );
    hanging.process_timeout = Duration::from_millis(100);
    let timeout = run_loader_smoke(hanging).await.unwrap_err().to_string();
    assert!(timeout.contains("hanging fixture did not exit within 0.1s."));
}
