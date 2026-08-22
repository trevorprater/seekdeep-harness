//! Real Loader composition, rollback, and joined teardown parity.

use std::{collections::BTreeMap, sync::Arc};

use seekdeep_cordis::{Context, Plugin};
use seekdeep_host_directory_picker::{DIRECTORY_PICKER, DirectoryPickerCapability};
use seekdeep_host_directory_picker_auto::{
    BROWSE_BACKEND_PACKAGE, BROWSE_SURFACE_PACKAGE, NATIVE_BACKEND_PACKAGE, NATIVE_SURFACE_PACKAGE,
    plugin,
};
use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig};
use seekdeep_loader::{LOADER, PluginCatalog};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};

const AUTO: &str = "@seekdeep-ai/seekdeep-host-directory-picker-auto";

fn web_plugin() -> Plugin {
    Plugin::new(
        "host-webserver",
        std::iter::empty::<&str>(),
        |context, config| {
            Box::pin(async move {
                let host = match config.get("host").and_then(serde_json::Value::as_str) {
                    Some("127.0.0.1") => ListenHost::Loopback,
                    Some("0.0.0.0") => ListenHost::AllInterfaces,
                    value => anyhow::bail!("invalid test host {value:?}"),
                };
                WebServer::install(&context, WebServerConfig { host, port: 0 }).await?;
                Ok(())
            })
        },
    )
}

fn surface_plugin(name: &'static str, fail: bool) -> Plugin {
    Plugin::new(name, std::iter::empty::<&str>(), move |_, _| {
        Box::pin(async move {
            if fail {
                anyhow::bail!("surface import failed: {name}")
            }
            Ok(())
        })
    })
}

fn attended_environment() -> (tempfile::TempDir, BTreeMap<String, String>) {
    let binary = tempfile::tempdir().unwrap();
    let zenity = binary.path().join("zenity");
    std::fs::write(&zenity, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&zenity, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (
        binary,
        BTreeMap::from([
            (
                "PATH".to_owned(),
                zenity.parent().unwrap().to_string_lossy().into_owned(),
            ),
            ("DISPLAY".to_owned(), ":0".to_owned()),
            ("SSH_CONNECTION".to_owned(), String::new()),
            ("SSH_TTY".to_owned(), String::new()),
        ]),
    )
}

fn catalog(fail_native_surface: bool) -> anyhow::Result<PluginCatalog> {
    let catalog = PluginCatalog::new();
    catalog.register_named("web", web_plugin())?;
    catalog.register_named(AUTO, plugin())?;
    catalog.register_named(
        NATIVE_BACKEND_PACKAGE,
        seekdeep_host_directory_picker_native::plugin(),
    )?;
    catalog.register_named(
        BROWSE_BACKEND_PACKAGE,
        seekdeep_host_directory_picker_browse::plugin(),
    )?;
    catalog.register_named(
        NATIVE_SURFACE_PACKAGE,
        surface_plugin(NATIVE_SURFACE_PACKAGE, fail_native_surface),
    )?;
    catalog.register_named(
        BROWSE_SURFACE_PACKAGE,
        surface_plugin(BROWSE_SURFACE_PACKAGE, false),
    )?;
    Ok(catalog)
}

async fn load(
    host: &str,
    ssh: bool,
    fail_native_surface: bool,
) -> anyhow::Result<(
    Context,
    seekdeep_loader::LoadedComposition,
    tempfile::TempDir,
)> {
    let context = Context::new();
    let (binary, mut environment) = attended_environment();
    if ssh {
        environment.insert(
            "SSH_CONNECTION".to_owned(),
            "10.0.0.2 55 10.0.0.9 22".to_owned(),
        );
    }
    let snapshot = Arc::new(create_launch_environment_snapshot(&[
        LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: environment,
        },
    ]));
    context.provide(SEEKDEEP_LAUNCH_ENVIRONMENT, snapshot)?;
    let source = format!(
        "- id: web\n  name: web\n  config:\n    host: '{host}'\n- id: auto\n  name: '{AUTO}'\n"
    );
    let composition = catalog(fail_native_surface)?
        .load_yaml(&context, &source)
        .await?;
    Ok((context, composition, binary))
}

fn names(context: &Context) -> Vec<String> {
    context
        .get(LOADER)
        .unwrap()
        .entries()
        .unwrap()
        .into_iter()
        .map(|entry| entry.plugin.as_str().to_owned())
        .collect()
}

#[tokio::test]
async fn attended_loopback_mounts_both_native_faces_and_disposal_joins_their_removal()
-> anyhow::Result<()> {
    let (context, composition, _binary) = load("127.0.0.1", false, false).await?;
    assert!(names(&context).contains(&NATIVE_BACKEND_PACKAGE.to_owned()));
    assert!(names(&context).contains(&NATIVE_SURFACE_PACKAGE.to_owned()));
    assert!(!names(&context).contains(&BROWSE_BACKEND_PACKAGE.to_owned()));
    assert!(matches!(
        context.get(DIRECTORY_PICKER).unwrap().capability(),
        DirectoryPickerCapability::Native { .. }
    ));

    let auto = composition
        .fibers()
        .into_iter()
        .find(|fiber| fiber.entry_name().as_deref() == Some(AUTO))
        .unwrap();
    auto.dispose().await?;
    assert!(!names(&context).contains(&NATIVE_BACKEND_PACKAGE.to_owned()));
    assert!(!names(&context).contains(&NATIVE_SURFACE_PACKAGE.to_owned()));
    assert!(context.get(DIRECTORY_PICKER).is_none());
    composition.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn ssh_and_all_interfaces_mount_browse_faces() -> anyhow::Result<()> {
    for (host, ssh) in [("127.0.0.1", true), ("0.0.0.0", false)] {
        let (context, composition, _binary) = load(host, ssh, false).await?;
        let entries = names(&context);
        assert!(entries.contains(&BROWSE_BACKEND_PACKAGE.to_owned()));
        assert!(entries.contains(&BROWSE_SURFACE_PACKAGE.to_owned()));
        assert!(!entries.contains(&NATIVE_BACKEND_PACKAGE.to_owned()));
        assert!(matches!(
            context.get(DIRECTORY_PICKER).unwrap().capability(),
            DirectoryPickerCapability::Browse { .. }
        ));
        composition.dispose().await?;
    }
    Ok(())
}

#[tokio::test]
async fn failed_surface_rolls_back_the_backend_before_activation_error_returns() {
    let context = Context::new();
    let (binary, environment) = attended_environment();
    let snapshot = Arc::new(create_launch_environment_snapshot(&[
        LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: environment,
        },
    ]));
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, snapshot)
        .unwrap();
    let source = format!(
        "- id: web\n  name: web\n  config:\n    host: '127.0.0.1'\n- id: auto\n  name: '{AUTO}'\n"
    );
    let error = catalog(true)
        .unwrap()
        .load_yaml(&context, &source)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("surface import failed"));
    assert!(context.get(DIRECTORY_PICKER).is_none());
    drop(binary);
}

#[tokio::test]
async fn already_removed_backend_is_tolerated_when_auto_unloads() -> anyhow::Result<()> {
    let (context, composition, _binary) = load("127.0.0.1", false, false).await?;
    let loader = context.get(LOADER).unwrap();
    let backend = loader
        .entries()?
        .into_iter()
        .find(|entry| entry.plugin.as_str() == NATIVE_BACKEND_PACKAGE)
        .unwrap();
    loader.remove_entry(&backend.id).await?;
    let auto = composition
        .fibers()
        .into_iter()
        .find(|fiber| fiber.entry_name().as_deref() == Some(AUTO))
        .unwrap();
    auto.dispose().await?;
    assert!(!names(&context).contains(&NATIVE_SURFACE_PACKAGE.to_owned()));
    composition.dispose().await?;
    Ok(())
}
