//! Fake-SDK lifecycle, setup, validation, and helper parity.

use std::{collections::BTreeMap, sync::Arc};

use futures::stream;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Fiber};
use seekdeep_e2b::{
    E2bByteStream, E2bCommandResult, E2bCommands, E2bConfig, E2bCreateOptions, E2bEntryInfo,
    E2bFactoryService, E2bFileType, E2bFiles, E2bSandbox, E2bSandboxFactory, E2bSandboxFuture,
    E2bSandboxNotFound, e2b_control_envs, install, quote_e2b_shell_arg,
};
use seekdeep_llm::AbortSignal;

#[derive(Default)]
struct SandboxState {
    directories: Vec<String>,
    commands: Vec<(String, BTreeMap<String, String>)>,
    info: Option<E2bEntryInfo>,
    make_dir_error: Option<String>,
    command_error: Option<String>,
    kill_error: Option<anyhow::Error>,
    kills: usize,
}

struct FakeSandbox {
    id: String,
    state: Arc<Mutex<SandboxState>>,
}

#[async_trait::async_trait]
impl E2bFiles for FakeSandbox {
    async fn get_info(
        &self,
        path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bEntryInfo> {
        Ok(self.state.lock().info.clone().unwrap_or(E2bEntryInfo {
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            path: path.to_owned(),
            kind: E2bFileType::Directory,
            size: 0,
            mode: 0o755,
            modified_time: None,
            symlink_target: None,
            metadata: BTreeMap::new(),
        }))
    }

    async fn read_bytes(
        &self,
        _path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("unused read")
    }

    async fn read_stream(
        &self,
        _path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bByteStream> {
        Ok(E2bByteStream {
            stream: Box::pin(stream::empty()),
            cancel: Arc::new(|| {}),
        })
    }

    async fn list(
        &self,
        _path: &str,
        _depth: u32,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<E2bEntryInfo>> {
        Ok(Vec::new())
    }

    async fn make_dir(&self, path: &str, _signal: Option<&AbortSignal>) -> anyhow::Result<bool> {
        let mut state = self.state.lock();
        if let Some(error) = state.make_dir_error.take() {
            anyhow::bail!(error);
        }
        state.directories.push(path.to_owned());
        Ok(true)
    }

    async fn write(
        &self,
        _path: &str,
        _content: &str,
        _metadata: BTreeMap<String, String>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("unused write")
    }

    async fn rename(
        &self,
        _from: &str,
        _to: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bEntryInfo> {
        anyhow::bail!("unused rename")
    }

    async fn remove(&self, _path: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl E2bCommands for FakeSandbox {
    async fn run(
        &self,
        command: &str,
        env: BTreeMap<String, String>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult> {
        let mut state = self.state.lock();
        if let Some(error) = state.command_error.take() {
            anyhow::bail!(error);
        }
        state.commands.push((command.to_owned(), env));
        Ok(E2bCommandResult::default())
    }
}

#[async_trait::async_trait]
impl E2bSandbox for FakeSandbox {
    fn sandbox_id(&self) -> &str {
        &self.id
    }

    fn files(&self) -> Arc<dyn E2bFiles> {
        Arc::new(Self {
            id: self.id.clone(),
            state: self.state.clone(),
        })
    }

    fn commands(&self) -> Arc<dyn E2bCommands> {
        Arc::new(Self {
            id: self.id.clone(),
            state: self.state.clone(),
        })
    }

    async fn kill(&self) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        state.kills += 1;
        if let Some(error) = state.kill_error.take() {
            return Err(error);
        }
        Ok(())
    }
}

struct FakeFactory {
    create: Arc<dyn Fn(E2bCreateOptions) -> E2bSandboxFuture + Send + Sync>,
    options: Arc<Mutex<Vec<E2bCreateOptions>>>,
}

impl E2bSandboxFactory for FakeFactory {
    fn create(&self, options: E2bCreateOptions) -> E2bSandboxFuture {
        self.options.lock().push(options.clone());
        (self.create)(options)
    }
}

fn factory(sandbox: Arc<dyn E2bSandbox>) -> (Arc<FakeFactory>, Arc<Mutex<Vec<E2bCreateOptions>>>) {
    let options = Arc::new(Mutex::new(Vec::new()));
    (
        Arc::new(FakeFactory {
            create: Arc::new(move |_| {
                let sandbox = sandbox.clone();
                Box::pin(async move { Ok(sandbox) })
            }),
            options: options.clone(),
        }),
        options,
    )
}

fn context_with_factory(factory: Arc<dyn E2bSandboxFactory>) -> (Context, Arc<Fiber>) {
    let root = Context::new();
    E2bFactoryService::new(factory).provide(&root).unwrap();
    let fiber = Fiber::active_child("e2b-owner");
    (root.with_fiber(fiber.clone()), fiber)
}

#[test]
fn helpers_quote_opaque_values_and_override_control_home_freshly() {
    assert_eq!(quote_e2b_shell_arg("a'b $HOME"), "'a'\"'\"'b $HOME'");
    let first = e2b_control_envs(&BTreeMap::from([
        ("HOME".to_owned(), "/hostile".to_owned()),
        ("NPM_TOKEN".to_owned(), String::new()),
    ]));
    let second = e2b_control_envs(&BTreeMap::new());
    assert!(first["HOME"].starts_with("/.seekdeep-e2b-control-"));
    assert_ne!(first["HOME"], second["HOME"]);
    assert_eq!(first["NPM_TOKEN"], "");
}

#[tokio::test]
async fn creates_one_protected_shared_sandbox_and_kills_it_on_disposal() {
    let state = Arc::new(Mutex::new(SandboxState::default()));
    let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox {
        id: "sandbox-1".to_owned(),
        state: state.clone(),
    });
    let (factory, options) = factory(sandbox.clone());
    let (context, fiber) = context_with_factory(factory);
    let service = install(
        &context,
        E2bConfig {
            api_key: Some("test-key".to_owned()),
            ..E2bConfig::default()
        },
    )
    .unwrap();

    assert!(Arc::ptr_eq(&service.get_sandbox().await.unwrap(), &sandbox));
    assert_eq!(service.cwd(), "/home/user/workspace");
    assert_eq!(service.runtime_root(), "/home/user/workspace/.seekdeep-e2b");
    assert_eq!(
        options.lock().as_slice(),
        [E2bCreateOptions {
            api_key: "test-key".to_owned(),
            timeout_ms: 300_000.0,
            secure: true,
            kill_on_timeout: true,
        }]
    );
    assert_eq!(
        state.lock().directories,
        ["/home/user/workspace", "/home/user/workspace/.seekdeep-e2b"]
    );
    assert_eq!(state.lock().commands.len(), 1);
    assert!(state.lock().commands[0].1["HOME"].starts_with("/.seekdeep-e2b-control-"));

    fiber.dispose().await.unwrap();
    assert_eq!(state.lock().kills, 1);
    assert!(
        service
            .get_sandbox()
            .await
            .err()
            .expect("disposed service rejects")
            .to_string()
            .contains("disposing")
    );
}

#[tokio::test]
async fn disposal_racing_setup_rejects_acquisition_and_kills_once() {
    let state = Arc::new(Mutex::new(SandboxState::default()));
    let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox {
        id: "pending".to_owned(),
        state: state.clone(),
    });
    let (send, receive) = tokio::sync::oneshot::channel();
    let receive = Arc::new(tokio::sync::Mutex::new(Some(receive)));
    let options = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(FakeFactory {
        create: Arc::new({
            let receive = receive.clone();
            move |_| {
                let receive = receive.clone();
                let sandbox = sandbox.clone();
                Box::pin(async move {
                    receive.lock().await.take().expect("one creation").await??;
                    Ok(sandbox)
                })
            }
        }),
        options,
    });
    let (context, fiber) = context_with_factory(factory);
    let service = install(
        &context,
        E2bConfig {
            api_key: Some("key".to_owned()),
            ..E2bConfig::default()
        },
    )
    .unwrap();
    let acquisition = tokio::spawn({
        let service = service.clone();
        async move { service.get_sandbox().await }
    });
    let disposal = tokio::spawn(async move { fiber.dispose().await });
    tokio::task::yield_now().await;
    send.send(Ok::<(), anyhow::Error>(())).unwrap();
    assert!(
        acquisition
            .await
            .unwrap()
            .err()
            .expect("racing acquisition rejects")
            .to_string()
            .contains("disposing")
    );
    disposal.await.unwrap().unwrap();
    assert_eq!(state.lock().kills, 1);
}

#[tokio::test]
async fn setup_failures_rollback_once_and_preserve_the_primary_error() {
    for (setup, expected) in [("mkdir", "setup failed"), ("chmod", "chmod failed")] {
        let state = Arc::new(Mutex::new(SandboxState::default()));
        if setup == "mkdir" {
            state.lock().make_dir_error = Some(expected.to_owned());
        } else {
            state.lock().command_error = Some(expected.to_owned());
            state.lock().kill_error = Some(anyhow::anyhow!("cleanup failed"));
        }
        let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox {
            id: setup.to_owned(),
            state: state.clone(),
        });
        let (factory, _) = factory(sandbox);
        let (context, fiber) = context_with_factory(factory);
        let service = install(
            &context,
            E2bConfig {
                api_key: Some("key".to_owned()),
                ..E2bConfig::default()
            },
        )
        .unwrap();
        assert!(
            service
                .get_sandbox()
                .await
                .err()
                .expect("setup failure")
                .to_string()
                .contains(expected)
        );
        assert_eq!(state.lock().kills, 1);
        fiber.dispose().await.unwrap();
        assert_eq!(state.lock().kills, 1);
    }
}

#[tokio::test]
async fn rejects_non_directory_runtime_roots_and_ignores_already_deleted_teardown() {
    for info in [
        E2bEntryInfo {
            name: ".seekdeep-e2b".to_owned(),
            path: "/workspace/.seekdeep-e2b".to_owned(),
            kind: E2bFileType::File,
            size: 0,
            mode: 0o600,
            modified_time: None,
            symlink_target: None,
            metadata: BTreeMap::new(),
        },
        E2bEntryInfo {
            name: ".seekdeep-e2b".to_owned(),
            path: "/workspace/.seekdeep-e2b".to_owned(),
            kind: E2bFileType::Directory,
            size: 0,
            mode: 0o700,
            modified_time: None,
            symlink_target: Some("/tmp/redirected".to_owned()),
            metadata: BTreeMap::new(),
        },
    ] {
        let state = Arc::new(Mutex::new(SandboxState {
            info: Some(info),
            ..SandboxState::default()
        }));
        let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox {
            id: "bad-root".to_owned(),
            state: state.clone(),
        });
        let (factory, _) = factory(sandbox);
        let (context, fiber) = context_with_factory(factory);
        let service = install(
            &context,
            E2bConfig {
                api_key: Some("key".to_owned()),
                cwd: "/workspace".to_owned(),
                ..E2bConfig::default()
            },
        )
        .unwrap();
        assert!(
            service
                .get_sandbox()
                .await
                .err()
                .expect("invalid runtime root")
                .to_string()
                .contains("runtime root must be a real directory")
        );
        assert!(state.lock().commands.is_empty());
        fiber.dispose().await.unwrap();
    }

    let state = Arc::new(Mutex::new(SandboxState {
        kill_error: Some(
            E2bSandboxNotFound {
                message: "already deleted".to_owned(),
            }
            .into(),
        ),
        ..SandboxState::default()
    }));
    let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox {
        id: "gone".to_owned(),
        state: state.clone(),
    });
    let (factory, _) = factory(sandbox);
    let (context, fiber) = context_with_factory(factory);
    let service = install(
        &context,
        E2bConfig {
            api_key: Some("key".to_owned()),
            ..E2bConfig::default()
        },
    )
    .unwrap();
    service.get_sandbox().await.unwrap();
    fiber.dispose().await.unwrap();
    assert_eq!(state.lock().kills, 1);
}

#[test]
fn validates_key_absolute_cwd_and_positive_finite_timeout_before_creation() {
    let state = Arc::new(Mutex::new(SandboxState::default()));
    let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox {
        id: "unused".to_owned(),
        state,
    });
    for (config, expected) in [
        (
            E2bConfig {
                api_key: Some(String::new()),
                ..E2bConfig::default()
            },
            "configure apiKey",
        ),
        (
            E2bConfig {
                api_key: Some("x".to_owned()),
                cwd: "relative".to_owned(),
                ..E2bConfig::default()
            },
            "absolute Linux path",
        ),
        (
            E2bConfig {
                api_key: Some("x".to_owned()),
                timeout_ms: 0.0,
                ..E2bConfig::default()
            },
            "positive finite",
        ),
    ] {
        let (factory, options) = factory(sandbox.clone());
        let (context, _fiber) = context_with_factory(factory);
        assert!(
            install(&context, config)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
        assert!(options.lock().is_empty());
    }
}
