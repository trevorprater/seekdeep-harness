//! Web startup flags, trust derivation, and runtime glue parity.

use std::sync::{Arc, Mutex};

use seekdeep_cordis::Context;
use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig};
use seekdeep_system_prompt::{AssembleContext, SystemPromptConfig, render_prompt};
use seekdeep_web_app::{
    Config, INJECT, SEEKDEEP_WEB_URL, WEB_RUNTIME, install_with_dist_index,
    install_with_runtime_seams, plugin, resolve_lan_trust,
    startup::{WebStartupOutcome, WebStartupValues, parse_web_startup},
};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

fn stage_dist() -> anyhow::Result<(tempfile::TempDir, std::path::PathBuf)> {
    let temporary = tempfile::tempdir()?;
    let dist = temporary.path().join("dist");
    std::fs::create_dir(&dist)?;
    let index = dist.join("index.html");
    std::fs::write(&index, "<head></head><body>shell</body>")?;
    Ok((temporary, index))
}

#[test]
fn startup_publishes_flags_defaults_and_exact_safety_failures() {
    assert_eq!(
        parse_web_startup(&strings(&[
            "--host",
            "127.0.0.1",
            "--port",
            "8080",
            "--trusted-host",
            "lab.internal",
            "lab-2.internal",
            "--trusted-host",
            "10.0.0.9",
        ])),
        WebStartupOutcome::Values(WebStartupValues {
            host: Some("127.0.0.1".to_owned()),
            port: Some(8080),
            trusted_hosts: strings(&["lab.internal", "lab-2.internal", "10.0.0.9"]),
        })
    );
    assert_eq!(
        parse_web_startup(&[]),
        WebStartupOutcome::Values(WebStartupValues::default())
    );
    for (arguments, diagnostic) in [
        (
            strings(&["--port", "abc"]),
            "--port must be a number, got \"abc\"",
        ),
        (
            strings(&["--host", "0.0.0.0"]),
            "--host 0.0.0.0 is intentionally not supported yet for safety",
        ),
    ] {
        let WebStartupOutcome::Exit { code, stderr, .. } = parse_web_startup(&arguments) else {
            panic!("invalid Web flags must request exit");
        };
        assert_eq!(code, 1);
        assert!(stderr.contains(diagnostic));
    }
    let WebStartupOutcome::Exit { code, stdout, .. } = parse_web_startup(&strings(&["--help"]))
    else {
        panic!("help must request exit");
    };
    assert_eq!(code, 0);
    assert!(stdout.contains("seekdeep --profile web"));
    assert!(stdout.contains("--trusted-host"));
}

#[test]
fn lan_trust_uses_one_noninternal_ipv4_sample_and_preserves_extra_order() {
    assert_eq!(
        resolve_lan_trust(
            "0.0.0.0",
            &strings(&["harness.internal:3080"]),
            strings(&["192.168.1.5", "10.0.0.7"]),
        ),
        seekdeep_web_app::WebRuntimeValues {
            lan_addresses: strings(&["192.168.1.5", "10.0.0.7"]),
            trusted_hosts: strings(&["192.168.1.5", "10.0.0.7", "harness.internal:3080",]),
        }
    );
    assert_eq!(
        resolve_lan_trust(
            "127.0.0.1",
            &strings(&["lab.internal"]),
            strings(&["192.168.1.5"]),
        ),
        seekdeep_web_app::WebRuntimeValues {
            lan_addresses: Vec::new(),
            trusted_hosts: strings(&["lab.internal"]),
        }
    );
}

#[tokio::test]
async fn runtime_mounts_static_fallback_prompt_shell_variable_and_runtime_values()
-> anyhow::Result<()> {
    let (_temporary, index) = stage_dist()?;
    let context = Context::new();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await?;
    let prompt = seekdeep_system_prompt::install(&context, SystemPromptConfig::default())?;
    let shell =
        seekdeep_shell_env::apply(&context, &seekdeep_shell_env::ShellEnvConfig::default())?;
    install_with_dist_index(
        &context,
        &Config {
            print_url: false,
            surface_context: true,
            trusted_hosts: vec!["lab.internal".to_owned()],
        },
        index,
    )?;
    assert_eq!(
        context.get(WEB_RUNTIME).unwrap().as_ref(),
        &serde_json::json!({
            "lanAddresses": [],
            "trustedHosts": ["lab.internal"],
        })
    );
    let rendered = render_prompt(&prompt.assemble(AssembleContext::default()).await?)?;
    assert!(rendered.contains(&format!(
        "SeekDeep Harness Web GUI at http://127.0.0.1:{}",
        server.port()
    )));
    assert!(rendered.contains("pnpm run dev:web"));
    assert!(
        shell
            .list()
            .iter()
            .any(|entry| entry.key == SEEKDEEP_WEB_URL)
    );
    context.fiber().dispose().await?;
    Ok(())
}

#[tokio::test]
async fn optional_surface_services_and_synchronous_readiness_match_source_lifecycle()
-> anyhow::Result<()> {
    assert_eq!(INJECT, ["webServer"]);
    assert_eq!(plugin().inject(), ["webServer"]);
    let (_temporary, index) = stage_dist()?;
    let context = Context::new();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::AllInterfaces,
            port: 0,
        },
    )
    .await?;
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink_lines = lines.clone();
    install_with_runtime_seams(
        &context,
        &Config {
            print_url: true,
            surface_context: false,
            trusted_hosts: vec!["lab.internal".to_owned()],
        },
        index,
        strings(&["192.168.1.5"]),
        Arc::new(move |line| sink_lines.lock().unwrap().push(line)),
    )?;
    assert_eq!(
        context.get(WEB_RUNTIME).unwrap().as_ref(),
        &serde_json::json!({
            "lanAddresses": ["192.168.1.5"],
            "trustedHosts": ["192.168.1.5", "lab.internal"],
        })
    );
    assert_eq!(
        lines.lock().unwrap().as_slice(),
        [format!(
            "seekdeep web: http://127.0.0.1:{} (LAN: http://192.168.1.5:{})",
            server.port(),
            server.port()
        )]
    );
    context.fiber().dispose().await?;
    Ok(())
}
