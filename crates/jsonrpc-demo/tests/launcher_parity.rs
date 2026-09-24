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

#[tokio::test]
async fn compiled_catalog_materializes_source_include_patches_and_replay_route() {
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("session.jsonl");
    std::fs::write(
        &fixture,
        concat!(
            "{\"type\":\"session\",\"version\":0,\"id\":\"fixture\",\"createdAt\":0}\n",
            "{\"type\":\"assistant/chunk\",\"seq\":1,\"time\":0,\"data\":{\"turn\":1,\"step\":1,\"chunk\":{\"type\":\"finish\",\"reason\":{\"kind\":\"stop\"}}}}\n",
        ),
    )
    .unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/jsonrpc-agent/cordis.yml");
    let config = root.path().join("cordis.snapshot.yml");
    std::fs::write(
        &config,
        format!(
            concat!(
                "- id: base\n",
                "  name: '@seekdeep-ai/cordis-plugin-include'\n",
                "  config:\n",
                "    path: {}\n",
                "    patches:\n",
                "      - id: sdk-jsonrpc-server\n",
                "        disabled: true\n",
                "      - id: llm-deepseek\n",
                "        name: '@seekdeep-ai/seekdeep-llm-deepseek'\n",
                "        disabled: true\n",
                "      - insert:\n",
                "          - id: llm-replay\n",
                "            name: '@seekdeep-ai/seekdeep-llm-replay'\n",
                "            config:\n",
                "              file: {}\n",
                "              providers:\n",
                "                - id: deepseek-official\n",
            ),
            serde_json::to_string(&source.to_string_lossy()).unwrap(),
            serde_json::to_string(&fixture.to_string_lossy()).unwrap(),
        ),
    )
    .unwrap();
    let mut environment = std::env::vars().collect::<BTreeMap<_, _>>();
    environment.insert(
        "SEEKDEEP_CWD".to_owned(),
        root.path().to_string_lossy().into_owned(),
    );
    environment.insert(
        "SEEKDEEP_SESSION_ROOT".to_owned(),
        root.path().join("sessions").to_string_lossy().into_owned(),
    );
    environment.insert(
        "SEEKDEEP_HOME".to_owned(),
        root.path().join("home").to_string_lossy().into_owned(),
    );
    let catalog =
        seekdeep_sdk_jsonrpc_demo::runner::catalog(root.path(), &environment, None).unwrap();
    let context = seekdeep_cordis::Context::new();
    let composition = catalog
        .load_yaml_at(
            &context,
            &std::fs::read_to_string(&config).unwrap(),
            &config,
        )
        .await
        .unwrap();
    let llm = context.get(seekdeep_llm::LLM).unwrap();
    assert_eq!(
        llm.list_providers()
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>(),
        ["deepseek-official"]
    );
    composition.dispose().await.unwrap();

    let application = seekdeep_app_boot::boot(
        "jsonrpc-catalog-test",
        &config,
        &catalog,
        seekdeep_app_boot::BootOptions::default(),
    )
    .await
    .unwrap();
    let llm = application.context().get(seekdeep_llm::LLM).unwrap();
    assert_eq!(
        llm.list_providers()
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>(),
        ["deepseek-official"]
    );
    application.dispose().await.unwrap();
}
