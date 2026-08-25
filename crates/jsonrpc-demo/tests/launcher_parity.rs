//! Generic and packaged process acceptance plus config-selection parity.

use std::{
    collections::BTreeMap,
    process::{Command, Stdio},
};

use seekdeep_sdk_jsonrpc_demo::runner::{CONFIG_ENV, selected_config_path, usage};

#[test]
fn environment_wins_argument_and_empty_values_are_absent() {
    let root = tempfile::tempdir().unwrap();
    let from_env = root.path().join("env.yml");
    let from_arg = root.path().join("arg.yml");
    std::fs::write(&from_env, "[]\n").unwrap();
    std::fs::write(&from_arg, "[]\n").unwrap();
    let environment = BTreeMap::from([(
        CONFIG_ENV.to_owned(),
        from_env.to_string_lossy().into_owned(),
    )]);
    assert_eq!(
        selected_config_path(
            &environment,
            &[from_arg.to_string_lossy().into_owned()],
            root.path(),
        )
        .unwrap(),
        from_env
    );
    assert_eq!(
        selected_config_path(
            &BTreeMap::from([(CONFIG_ENV.to_owned(), String::new())]),
            &[from_arg.to_string_lossy().into_owned()],
            root.path(),
        )
        .unwrap(),
        from_arg
    );
    assert_eq!(
        selected_config_path(&BTreeMap::new(), &[], root.path())
            .unwrap_err()
            .to_string(),
        usage()
    );
}

fn assert_binary(binary: &str) {
    let missing = Command::new(binary).stdin(Stdio::null()).output().unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains(&usage()));

    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("cordis.yml");
    std::fs::write(&config, "[]\n").unwrap();
    let success = Command::new(binary)
        .arg(&config)
        .current_dir(root.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        success.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&success.stderr)
    );

    let override_success = Command::new(binary)
        .arg(root.path().join("missing.yml"))
        .env(CONFIG_ENV, &config)
        .current_dir(root.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(override_success.status.code(), Some(0));
}

#[test]
fn generic_and_packaged_binaries_require_external_config_and_own_stdin_eof() {
    assert_binary(env!("CARGO_BIN_EXE_seekdeep-jsonrpc-agent"));
    assert_binary(env!("CARGO_BIN_EXE_seekdeep-jsonrpc-agent-packaged"));
}
