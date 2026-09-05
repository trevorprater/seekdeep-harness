//! Real loader composition for LLM, settings, credentials, and `DeepSeek`.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures::TryStreamExt as _;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_credentials::{CREDENTIALS, credential_ref};
use seekdeep_credentials_local::plugin as credentials_plugin;
use seekdeep_llm::{
    ContentBlock, GenerateOptions, LlmRuntime, Message, MessageRole, MessageSource,
};
use seekdeep_llm_deepseek::plugin as deepseek_plugin;
use seekdeep_loader::{LoadedComposition, PluginCatalog};
use seekdeep_settings::{SETTINGS, settings_namespace};
use seekdeep_settings_file::plugin as settings_plugin;
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

#[derive(Clone)]
struct CapturedRequest {
    headers: BTreeMap<String, String>,
}

struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    task: tokio::task::AbortHandle,
}

impl MockServer {
    async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let capture = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let capture = capture.clone();
                tokio::spawn(async move {
                    let _ = respond(stream, capture).await;
                });
            }
        });
        let task = task.abort_handle();
        Self {
            url: format!("http://{address}"),
            requests,
            task,
        }
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
) -> anyhow::Result<()> {
    let request = read_request(&mut stream).await?;
    requests.lock().push(request);
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(boundary) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break boundary + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary])?;
    let mut headers = BTreeMap::new();
    for line in head.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("malformed request header"))?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < boundary + length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(CapturedRequest { headers })
}

struct Composition {
    context: Context,
    loaded: LoadedComposition,
    settings_path: PathBuf,
    credentials_path: PathBuf,
}

fn llm_plugin() -> Plugin {
    Plugin::new("llm", std::iter::empty::<&str>(), |context, _| {
        Box::pin(async move {
            LlmRuntime::install(&context)?;
            Ok(())
        })
    })
}

fn catalog() -> PluginCatalog {
    let catalog = PluginCatalog::new();
    catalog
        .register_named("test-llm-service", llm_plugin())
        .unwrap();
    catalog
        .register_named("seekdeep-settings-file", settings_plugin())
        .unwrap();
    catalog
        .register_named("seekdeep-credentials-local", credentials_plugin())
        .unwrap();
    catalog
        .register_named("seekdeep-llm-deepseek", deepseek_plugin())
        .unwrap();
    catalog
}

fn environment(
    home: &Path,
    key: &str,
) -> Arc<seekdeep_util::launch_environment::LaunchEnvironmentSnapshot> {
    Arc::new(create_launch_environment_snapshot(&[
        LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: BTreeMap::from([
                (
                    "SEEKDEEP_HOME".to_owned(),
                    home.to_string_lossy().into_owned(),
                ),
                ("DEEPSEEK_API_KEY".to_owned(), key.to_owned()),
            ]),
        },
    ]))
}

async fn write_private(path: &Path, value: &str) {
    tokio::fs::write(path, value).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }
}

async fn load_composition(
    home: &Path,
    with_dynamic: bool,
    fresh: bool,
    base_url: &str,
    environment_key: &str,
) -> Composition {
    let settings_path = home.join("settings.yaml");
    let credentials_path = home.join(".credentials.yaml");
    if with_dynamic && fresh {
        tokio::fs::write(&settings_path, "# personal settings\n")
            .await
            .unwrap();
        write_private(&credentials_path, "DEEPSEEK_API_KEY: boot-key\n").await;
    }
    let mut source = String::from("- id: llm\n  name: test-llm-service\n");
    if with_dynamic {
        write!(
            source,
            concat!(
                "- id: settings\n",
                "  name: seekdeep-settings-file\n",
                "  config:\n",
                "    path: {}\n",
                "    debounceMs: 5\n",
                "- id: credentials\n",
                "  name: seekdeep-credentials-local\n",
                "  config:\n",
                "    path: {}\n",
                "    debounceMs: 5\n",
            ),
            serde_json::to_string(&settings_path).unwrap(),
            serde_json::to_string(&credentials_path).unwrap(),
        )
        .unwrap();
    }
    writeln!(
        source,
        "- id: llm-deepseek\n  name: seekdeep-llm-deepseek\n  config:\n    baseURL: {}",
        serde_json::to_string(base_url).unwrap()
    )
    .unwrap();
    let context = Context::new();
    context
        .provide(
            SEEKDEEP_LAUNCH_ENVIRONMENT,
            environment(home, environment_key),
        )
        .unwrap();
    let loaded = catalog().load_yaml(&context, &source).await.unwrap();
    Composition {
        context,
        loaded,
        settings_path,
        credentials_path,
    }
}

fn request() -> GenerateOptions {
    GenerateOptions::new(
        seekdeep_llm::ProviderId::new("deepseek-official"),
        seekdeep_llm::ModelId::new("deepseek-v4-flash"),
        vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "hi".to_owned(),
            }],
            MessageSource::plugin("loader-test"),
        )],
    )
}

async fn prompt(context: &Context) {
    context
        .get(seekdeep_llm::LLM)
        .unwrap()
        .stream(request())
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_credential(context: &Context, expected: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let resolved = context
                .get(CREDENTIALS)
                .unwrap()
                .resolve(&credential_ref("DEEPSEEK_API_KEY").unwrap())
                .await
                .unwrap();
            if resolved.is_some_and(|credential| credential.value == expected) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn yaml_composition_routes_after_external_settings_and_credential_edits() {
    let home = tempfile::tempdir().unwrap();
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let composition = load_composition(home.path(), true, true, &first.url, "").await;
    assert_eq!(
        composition.context.get(SETTINGS).unwrap().describe(false)[0]
            .ns
            .as_str(),
        "llm-deepseek"
    );
    prompt(&composition.context).await;
    assert_eq!(
        first.requests()[0].headers["authorization"],
        "Bearer boot-key"
    );

    tokio::fs::write(
        &composition.settings_path,
        format!("llm-deepseek:\n  baseURL: {}\n", second.url),
    )
    .await
    .unwrap();
    wait_until(|| {
        let value = composition
            .context
            .get(SETTINGS)
            .unwrap()
            .get(&settings_namespace("llm-deepseek").unwrap());
        value.as_ref().and_then(|value| value["baseURL"].as_str()) == Some(second.url.as_str())
    })
    .await;
    write_private(
        &composition.credentials_path,
        "DEEPSEEK_API_KEY: rotated-key\n",
    )
    .await;
    wait_for_credential(&composition.context, "rotated-key").await;
    prompt(&composition.context).await;
    assert_eq!(first.requests().len(), 1);
    assert_eq!(
        second.requests()[0].headers["authorization"],
        "Bearer rotated-key"
    );
    composition.loaded.dispose().await.unwrap();
}

#[tokio::test]
async fn stored_key_remains_writable_and_rotatable_across_loader_restart() {
    let home = tempfile::tempdir().unwrap();
    let first_server = MockServer::start().await;
    let first = load_composition(home.path(), true, true, &first_server.url, "").await;
    let reference = credential_ref("DEEPSEEK_API_KEY").unwrap();
    first
        .context
        .get(CREDENTIALS)
        .unwrap()
        .set(&reference, "stored-by-ui")
        .await
        .unwrap();
    prompt(&first.context).await;
    assert_eq!(
        first_server.requests()[0].headers["authorization"],
        "Bearer stored-by-ui"
    );
    first.loaded.dispose().await.unwrap();

    let second_server = MockServer::start().await;
    let second = load_composition(home.path(), true, false, &second_server.url, "").await;
    let credentials = second.context.get(CREDENTIALS).unwrap();
    assert_eq!(
        credentials
            .resolve(&reference)
            .await
            .unwrap()
            .unwrap()
            .value,
        "stored-by-ui"
    );
    credentials
        .set(&reference, "rotated-after-restart")
        .await
        .unwrap();
    prompt(&second.context).await;
    assert_eq!(
        second_server.requests()[0].headers["authorization"],
        "Bearer rotated-after-restart"
    );
    second.loaded.dispose().await.unwrap();
}

#[tokio::test]
async fn entry_only_composition_resolves_credential_reference_from_environment() {
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let composition = load_composition(home.path(), false, true, &server.url, "entry-key").await;
    assert!(composition.context.get(SETTINGS).is_none());
    assert!(composition.context.get(CREDENTIALS).is_none());
    prompt(&composition.context).await;
    assert_eq!(
        server.requests()[0].headers["authorization"],
        "Bearer entry-key"
    );
    composition.loaded.dispose().await.unwrap();
}
