//! Adapter from the official Codex CLI ChatGPT session to provider credentials.

use std::{
    future::Future,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{SecondsFormat, Utc};
use path_clean::PathClean as _;
use seekdeep_llm::ProviderId;
use seekdeep_util::{
    atomic_write::{FileLockError, WriteFileAtomicOptions, with_file_lock, write_file_atomic},
    launch_environment::LaunchEnvironmentSnapshot,
};
use serde_json::{Map, Value};

/// Installed route identity for `ChatGPT` subscription access.
pub const OPENAI_CODEX_PROVIDER_ID: &str = "openai-codex";
const CODEX_HOME_ENV: &str = "CODEX_HOME";
const CODEX_AUTH_FILENAME: &str = "auth.json";
const CHATGPT_AUTH_MODE: &str = "chatgpt";
const OPENAI_AUTH_CLAIM: &str = "https://api.openai.com/auth";
const OPENAI_CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const MAX_CODEX_AUTH_BYTES: u64 = 1024 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// OAuth credential shape expected by the provider SDK compatibility layer.
#[derive(Clone, Debug, PartialEq)]
pub struct OAuthCredential {
    /// Bearer access token.
    pub access: String,
    /// Refresh token.
    pub refresh: String,
    /// Access expiry in Unix milliseconds.
    pub expires: f64,
    /// Selected `ChatGPT` account.
    pub account_id: Option<String>,
}

/// Extensible credential input accepted from a refresh callback.
#[derive(Clone, Debug, PartialEq)]
pub enum Credential {
    /// OAuth credential.
    OAuth(OAuthCredential),
    /// API key, rejected by this OAuth-only bridge.
    ApiKey {
        /// Secret API key.
        key: String,
    },
}

/// Secret-free credential listing row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialInfo {
    /// Provider route.
    pub provider_id: ProviderId,
    /// Credential kind.
    pub credential_type: &'static str,
}

#[derive(Clone, Debug)]
struct CodexAuthDocument {
    root: Map<String, Value>,
    tokens: Map<String, Value>,
    credential: OAuthCredential,
}

/// Resolved official Codex credential bridge.
#[derive(Clone, Debug)]
pub struct CodexCredentialBridge {
    /// Provider-scoped file store.
    pub store: CodexFileCredentialStore,
    /// Absolute path used for filesystem access.
    pub file_path: PathBuf,
    /// Symbolic, secret-free path used in diagnostics.
    pub display_path: String,
}

/// `OpenAI` Codex OAuth refresh transport used by the provider auth resolver.
#[derive(Clone, Debug)]
pub struct CodexOAuthRefresher {
    http: reqwest::Client,
    token_url: String,
}

impl CodexOAuthRefresher {
    /// Creates the production `OpenAI` token refresher.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            token_url: OPENAI_CODEX_TOKEN_URL.to_owned(),
        }
    }

    #[cfg(test)]
    fn with_token_url(http: reqwest::Client, token_url: impl Into<String>) -> Self {
        Self {
            http,
            token_url: token_url.into(),
        }
    }

    async fn refresh(&self, current: &OAuthCredential) -> anyhow::Result<Credential> {
        let response = self
            .http
            .post(&self.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", current.refresh.as_str()),
                ("client_id", OPENAI_CODEX_CLIENT_ID),
            ])
            .send()
            .await
            .map_err(|error| anyhow::anyhow!("OpenAI Codex token refresh error: {error}"))?;
        let status = response.status();
        let status_text = status.canonical_reason().unwrap_or_default().to_owned();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let detail = if body.is_empty() { status_text } else { body };
            anyhow::bail!(
                "OpenAI Codex token refresh failed ({}): {detail}",
                status.as_u16()
            );
        }
        let value: Value = serde_json::from_str(&body).map_err(|error| {
            anyhow::anyhow!("OpenAI Codex token refresh response was not valid JSON: {error}")
        })?;
        let access = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let refresh = value
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let expires_in = value.get("expires_in").and_then(Value::as_f64);
        let (Some(access), Some(refresh), Some(expires_in)) = (access, refresh, expires_in) else {
            anyhow::bail!(
                "OpenAI Codex token refresh response missing fields: {}",
                serde_json::to_string(&value)?
            );
        };
        let account_id = access_token_account_id(access)
            .ok_or_else(|| anyhow::anyhow!("Failed to extract accountId from token"))?;
        Ok(Credential::OAuth(OAuthCredential {
            access: access.to_owned(),
            refresh: refresh.to_owned(),
            expires: unix_millis() + expires_in * 1000.0,
            account_id: Some(account_id),
        }))
    }
}

impl CodexCredentialBridge {
    /// Resolves the current Codex OAuth credential, refreshing and persisting
    /// it under the shared writer lock when its access token has expired.
    ///
    /// # Errors
    ///
    /// Returns bounded-read, validation, refresh, lock, or atomic-write failures.
    pub async fn resolve_oauth(
        &self,
        refresher: &CodexOAuthRefresher,
    ) -> anyhow::Result<Option<OAuthCredential>> {
        let Some(Credential::OAuth(stored)) = self.store.read(OPENAI_CODEX_PROVIDER_ID).await?
        else {
            return Ok(None);
        };
        if unix_millis() < stored.expires {
            return Ok(Some(stored));
        }
        let refresher = refresher.clone();
        let post = self
            .store
            .modify(OPENAI_CODEX_PROVIDER_ID, move |current| async move {
                let Some(Credential::OAuth(current)) = current else {
                    return Ok(None);
                };
                if unix_millis() < current.expires {
                    return Ok(None);
                }
                refresher.refresh(&current).await.map(Some)
            })
            .await?;
        Ok(match post {
            Some(Credential::OAuth(credential)) => Some(credential),
            Some(Credential::ApiKey { .. }) => {
                unreachable!("Codex credential bridge is OAuth-only")
            }
            None => None,
        })
    }
}

fn unix_millis() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

fn access_token_account_id(access: &str) -> Option<String> {
    let mut parts = access.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims
        .get(OPENAI_AUTH_CLAIM)?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// File-backed store exposing only the official Codex `ChatGPT` credential.
#[derive(Clone, Debug)]
pub struct CodexFileCredentialStore {
    filename: PathBuf,
    display_path: String,
}

impl CodexFileCredentialStore {
    /// Reads a provider credential, returning none for every non-Codex route.
    ///
    /// # Errors
    ///
    /// Returns bounded-read, permission, JSON, or credential validation failures.
    pub async fn read(&self, provider_id: &str) -> anyhow::Result<Option<Credential>> {
        if provider_id != OPENAI_CODEX_PROVIDER_ID {
            return Ok(None);
        }
        Ok(read_codex_auth(&self.filename, &self.display_path)
            .await?
            .map(|document| Credential::OAuth(document.credential)))
    }

    /// Lists the one supported credential when a valid login exists.
    ///
    /// # Errors
    ///
    /// Returns the same boundary failures as [`Self::read`].
    pub async fn list(&self) -> anyhow::Result<Vec<CredentialInfo>> {
        if self.read(OPENAI_CODEX_PROVIDER_ID).await?.is_none() {
            Ok(Vec::new())
        } else {
            Ok(vec![CredentialInfo {
                provider_id: ProviderId::new(OPENAI_CODEX_PROVIDER_ID),
                credential_type: "oauth",
            }])
        }
    }

    /// Runs one provider-SDK credential update under the shared writer lock.
    ///
    /// The official Codex CLI retains login, logout, and concurrent-refresh
    /// ownership. A concurrent Codex rotation wins over either a callback result
    /// or a reused-token callback failure.
    ///
    /// # Errors
    ///
    /// Returns callback, validation, lock, read, or atomic-write failures.
    pub async fn modify<F, Fut>(
        &self,
        provider_id: &str,
        update: F,
    ) -> anyhow::Result<Option<Credential>>
    where
        F: FnOnce(Option<Credential>) -> Fut,
        Fut: Future<Output = anyhow::Result<Option<Credential>>>,
    {
        if provider_id != OPENAI_CODEX_PROVIDER_ID {
            let unsupported = update(None).await?;
            anyhow::ensure!(
                unsupported.is_none(),
                "llm-pi-ai: the Codex credential bridge does not store provider {provider_id}"
            );
            return Ok(None);
        }
        if read_codex_auth(&self.filename, &self.display_path)
            .await?
            .is_none()
        {
            let created = update(None).await?;
            anyhow::ensure!(
                created.is_none(),
                "llm-pi-ai: run codex login to create {}",
                self.display_path
            );
            return Ok(None);
        }

        let filename = self.filename.clone();
        let lock_target = filename.clone();
        let display_path = self.display_path.clone();
        match with_file_lock(&lock_target, || async move {
            modify_locked(&filename, &display_path, update).await
        })
        .await
        {
            Ok(value) => Ok(value),
            Err(FileLockError::Operation(error)) => Err(error),
            Err(FileLockError::Acquire(error) | FileLockError::Release(error)) => Err(error.into()),
            Err(FileLockError::Timeout { path }) => anyhow::bail!(
                "atomic-write: timed out waiting for the writer lock at {}",
                path.display()
            ),
        }
    }

    /// Leaves deletion of the shared credential to `codex logout`.
    ///
    /// # Errors
    ///
    /// Rejects deletion of the Codex route.
    #[allow(clippy::unused_async)]
    pub async fn delete(&self, provider_id: &str) -> anyhow::Result<()> {
        if provider_id == OPENAI_CODEX_PROVIDER_ID {
            anyhow::bail!("llm-pi-ai: run codex logout to remove the shared ChatGPT credential");
        }
        Ok(())
    }
}

async fn modify_locked<F, Fut>(
    filename: &Path,
    display_path: &str,
    update: F,
) -> anyhow::Result<Option<Credential>>
where
    F: FnOnce(Option<Credential>) -> Fut,
    Fut: Future<Output = anyhow::Result<Option<Credential>>>,
{
    let Some(before) = read_codex_auth(filename, display_path).await? else {
        return Ok(None);
    };
    let candidate = match update(Some(Credential::OAuth(before.credential.clone()))).await {
        Ok(candidate) => candidate,
        Err(error) => {
            let concurrent = read_codex_auth(filename, display_path).await?;
            if concurrent.as_ref().is_some_and(|concurrent| {
                credential_changed(&before.credential, &concurrent.credential)
            }) {
                return Ok(concurrent.map(|value| Credential::OAuth(value.credential)));
            }
            return Err(error);
        }
    };
    let Some(candidate) = candidate else {
        return Ok(Some(Credential::OAuth(before.credential)));
    };
    let next = normalized_oauth_credential(candidate, display_path)?;
    anyhow::ensure!(
        next.account_id == before.credential.account_id,
        "llm-pi-ai: refreshed OAuth credential for {display_path} changed account identity"
    );
    let Some(latest) = read_codex_auth(filename, display_path).await? else {
        return Ok(None);
    };
    if credential_changed(&before.credential, &latest.credential) {
        return Ok(Some(Credential::OAuth(latest.credential)));
    }
    let mut tokens = latest.tokens;
    tokens.insert("access_token".into(), Value::String(next.access.clone()));
    tokens.insert("refresh_token".into(), Value::String(next.refresh.clone()));
    tokens.insert(
        "account_id".into(),
        next.account_id.clone().map_or(Value::Null, Value::String),
    );
    let mut root = latest.root;
    root.insert("tokens".into(), Value::Object(tokens));
    root.insert(
        "last_refresh".into(),
        Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
    );
    let rendered = serde_json::to_vec_pretty(&Value::Object(root))?;
    write_file_atomic(
        filename,
        &rendered,
        WriteFileAtomicOptions {
            mode: 0o600,
            dir_mode: Some(0o700),
        },
    )
    .await?;
    Ok(Some(Credential::OAuth(next)))
}

fn normalized_oauth_credential(
    value: Credential,
    location: &str,
) -> anyhow::Result<OAuthCredential> {
    let Credential::OAuth(value) = value else {
        anyhow::bail!("llm-pi-ai: {location} accepts only an OAuth credential");
    };
    anyhow::ensure!(
        !value.access.is_empty()
            && !value.refresh.is_empty()
            && value.expires.is_finite()
            && value.expires > 0.0,
        "llm-pi-ai: refreshed OAuth credential for {location} is incomplete"
    );
    let (account_id, expires) = access_token_claims(&value.access, location)?;
    if let Some(supplied) = value.account_id.as_ref() {
        anyhow::ensure!(
            supplied == &account_id,
            "llm-pi-ai: refreshed OAuth credential for {location} changed account identity"
        );
    }
    Ok(OAuthCredential {
        access: value.access,
        refresh: value.refresh,
        expires,
        account_id: Some(account_id),
    })
}

fn credential_changed(before: &OAuthCredential, after: &OAuthCredential) -> bool {
    before.access != after.access || before.refresh != after.refresh
}

async fn read_codex_auth(
    filename: &Path,
    display_path: &str,
) -> anyhow::Result<Option<CodexAuthDocument>> {
    let metadata = match tokio::fs::metadata(filename).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file(),
        "llm-pi-ai: {display_path} is not a regular file"
    );
    ensure_owner_only(&metadata, display_path)?;
    anyhow::ensure!(
        metadata.len() <= MAX_CODEX_AUTH_BYTES,
        "llm-pi-ai: {display_path} exceeds the {MAX_CODEX_AUTH_BYTES}-byte limit"
    );
    for attempt in 0..2 {
        let source = match tokio::fs::read(filename).await {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        match serde_json::from_slice::<Value>(&source) {
            Ok(value) => return parse_codex_auth(value, display_path),
            Err(_) if attempt == 0 => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(_) => anyhow::bail!("llm-pi-ai: {display_path} is not valid JSON"),
        }
    }
    unreachable!("the bounded parse loop returns or fails")
}

#[cfg(unix)]
#[allow(clippy::verbose_bit_mask)]
fn ensure_owner_only(metadata: &std::fs::Metadata, display_path: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    anyhow::ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "llm-pi-ai: {display_path} must be owner-only (mode 0600)"
    );
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only(_: &std::fs::Metadata, _: &str) -> anyhow::Result<()> {
    Ok(())
}

fn parse_codex_auth(value: Value, location: &str) -> anyhow::Result<Option<CodexAuthDocument>> {
    let Value::Object(root) = value else {
        anyhow::bail!("llm-pi-ai: {location} must contain a JSON object");
    };
    if let Some(mode) = root.get("auth_mode") {
        anyhow::ensure!(
            mode.is_string(),
            "llm-pi-ai: {location} auth_mode must be a string"
        );
    }
    if root.get("auth_mode").and_then(Value::as_str) != Some(CHATGPT_AUTH_MODE) {
        return Ok(None);
    }
    let tokens = root
        .get("tokens")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: {location} must contain ChatGPT tokens"))?;
    required_string(&tokens, "id_token", location)?;
    let access = required_string(&tokens, "access_token", location)?.to_owned();
    let refresh = required_string(&tokens, "refresh_token", location)?.to_owned();
    let (account_id, expires) = access_token_claims(&access, location)?;
    if let Some(stored) = tokens.get("account_id")
        && !stored.is_null()
    {
        let stored = stored
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "llm-pi-ai: {location} account_id must be a non-empty string or null"
                )
            })?;
        anyhow::ensure!(
            stored == account_id,
            "llm-pi-ai: {location} account_id does not match its access_token"
        );
    }
    Ok(Some(CodexAuthDocument {
        root,
        tokens,
        credential: OAuthCredential {
            access,
            refresh,
            expires,
            account_id: Some(account_id),
        },
    }))
}

fn required_string<'a>(
    record: &'a Map<String, Value>,
    field: &str,
    location: &str,
) -> anyhow::Result<&'a str> {
    record
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("llm-pi-ai: {location} must contain a non-empty {field}"))
}

fn access_token_claims(access: &str, location: &str) -> anyhow::Result<(String, f64)> {
    let mut parts = access.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        anyhow::bail!("llm-pi-ai: {location} contains an invalid access_token JWT");
    };
    anyhow::ensure!(
        !payload.is_empty(),
        "llm-pi-ai: {location} contains an invalid access_token JWT"
    );
    let decoded = URL_SAFE_NO_PAD.decode(payload).map_err(|_| {
        anyhow::anyhow!("llm-pi-ai: {location} contains an invalid access_token JWT")
    })?;
    let claims: Value = serde_json::from_slice(&decoded).map_err(|_| {
        anyhow::anyhow!("llm-pi-ai: {location} contains an invalid access_token JWT")
    })?;
    let claims = claims.as_object().ok_or_else(|| {
        anyhow::anyhow!("llm-pi-ai: {location} contains an invalid access_token JWT")
    })?;
    let expires = claims
        .get("exp")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && *value <= MAX_SAFE_INTEGER)
        .ok_or_else(|| {
            anyhow::anyhow!("llm-pi-ai: {location} access_token has no valid exp claim")
        })?;
    let account_id = claims
        .get(OPENAI_AUTH_CLAIM)
        .and_then(Value::as_object)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("llm-pi-ai: {location} access_token has no ChatGPT account id")
        })?;
    #[allow(clippy::cast_precision_loss)]
    let expires_millis = expires as f64 * 1000.0;
    Ok((account_id.to_owned(), expires_millis))
}

/// Resolves the official Codex file store from one immutable launch snapshot.
#[must_use]
pub fn create_codex_credential_bridge(
    environment: &LaunchEnvironmentSnapshot,
) -> CodexCredentialBridge {
    let configured = environment.get(CODEX_HOME_ENV).map(|entry| entry.value);
    let explicit = configured
        .as_deref()
        .is_some_and(|configured| !configured.trim().is_empty());
    let codex_home = if explicit {
        absolute_path(Path::new(configured.as_deref().unwrap_or_default()))
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".codex")
    };
    let file_path = codex_home.join(CODEX_AUTH_FILENAME);
    let display_path = if explicit {
        format!("${CODEX_HOME_ENV}/{CODEX_AUTH_FILENAME}")
    } else {
        format!("~/.codex/{CODEX_AUTH_FILENAME}")
    };
    CodexCredentialBridge {
        store: CodexFileCredentialStore {
            filename: file_path.clone(),
            display_path: display_path.clone(),
        },
        file_path,
        display_path,
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf().clean()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
            .clean()
    }
}

#[cfg(test)]
mod refresh_tests {
    use std::{collections::BTreeMap, sync::Arc};

    use parking_lot::Mutex;
    use seekdeep_util::launch_environment::{
        LaunchEnvironmentLayerInput, LaunchEnvironmentSource, create_launch_environment_snapshot,
    };
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::*;

    struct TokenServer {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        task: JoinHandle<()>,
    }

    impl TokenServer {
        async fn start(status: u16, body: Value) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let task_requests = requests.clone();
            let body = serde_json::to_string(&body).unwrap();
            let task = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let requests = task_requests.clone();
                    let body = body.clone();
                    tokio::spawn(async move {
                        let mut bytes = Vec::new();
                        let header_end = loop {
                            let mut buffer = [0_u8; 2048];
                            let Ok(read) = socket.read(&mut buffer).await else {
                                return;
                            };
                            if read == 0 {
                                return;
                            }
                            bytes.extend_from_slice(&buffer[..read]);
                            if let Some(index) =
                                bytes.windows(4).position(|window| window == b"\r\n\r\n")
                            {
                                break index + 4;
                            }
                        };
                        let headers = String::from_utf8_lossy(&bytes[..header_end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or_default();
                        while bytes.len() - header_end < length {
                            let mut buffer = vec![0_u8; length - (bytes.len() - header_end)];
                            let Ok(read) = socket.read(&mut buffer).await else {
                                return;
                            };
                            if read == 0 {
                                return;
                            }
                            bytes.extend_from_slice(&buffer[..read]);
                        }
                        requests.lock().push(
                            String::from_utf8_lossy(&bytes[header_end..header_end + length])
                                .into_owned(),
                        );
                        let response = format!(
                            "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                    });
                }
            });
            Self {
                url: format!("http://{address}/oauth/token"),
                requests,
                task,
            }
        }
    }

    impl Drop for TokenServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn jwt(account: &str, expires: u64) -> String {
        let encode = |value: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
        format!(
            "{}.{}.x",
            encode(json!({"alg":"none"})),
            encode(json!({"exp":expires,(OPENAI_AUTH_CLAIM):{"chatgpt_account_id":account}}))
        )
    }

    fn future_expiry() -> u64 {
        u64::try_from(Utc::now().timestamp()).unwrap() + 3600
    }

    fn auth_document(access: &str, refresh: &str) -> Value {
        json!({
            "auth_mode":"chatgpt",
            "OPENAI_API_KEY":null,
            "tokens":{
                "id_token":"id-token-must-survive",
                "access_token":access,
                "refresh_token":refresh,
                "account_id":"account-one",
                "future_token_field":"kept"
            },
            "last_refresh":"2026-01-01T00:00:00.000Z",
            "future_root_field":{"kept":true}
        })
    }

    async fn write_auth(home: &tempfile::TempDir, value: &Value) {
        let path = home.path().join("auth.json");
        tokio::fs::write(&path, serde_json::to_vec_pretty(value).unwrap())
            .await
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
        }
    }

    fn bridge(home: &Path) -> CodexCredentialBridge {
        let snapshot = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: BTreeMap::from([("CODEX_HOME".to_owned(), home.display().to_string())]),
        }]);
        create_codex_credential_bridge(&snapshot)
    }

    #[tokio::test]
    async fn expired_token_refreshes_once_under_lock_and_preserves_shared_document() {
        let home = tempfile::tempdir().unwrap();
        write_auth(&home, &auth_document(&jwt("account-one", 1), "refresh-old")).await;
        let next_access = jwt("account-one", future_expiry());
        let server = TokenServer::start(
            200,
            json!({
                "access_token":next_access,
                "refresh_token":"refresh-new",
                "expires_in":3600
            }),
        )
        .await;
        let refresher =
            CodexOAuthRefresher::with_token_url(reqwest::Client::new(), server.url.clone());
        let bridge = bridge(home.path());
        let (first, second) = tokio::join!(
            bridge.resolve_oauth(&refresher),
            bridge.resolve_oauth(&refresher)
        );
        assert_eq!(first.unwrap().unwrap().refresh, "refresh-new");
        assert_eq!(second.unwrap().unwrap().refresh, "refresh-new");
        assert_eq!(server.requests.lock().len(), 1);
        let form = server.requests.lock()[0].clone();
        assert!(form.contains("grant_type=refresh_token"));
        assert!(form.contains("refresh_token=refresh-old"));
        assert!(form.contains(&format!("client_id={OPENAI_CODEX_CLIENT_ID}")));

        let written: Value = serde_json::from_slice(
            &tokio::fs::read(home.path().join("auth.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(written["future_root_field"], json!({"kept":true}));
        assert_eq!(written["tokens"]["future_token_field"], "kept");
        assert_eq!(written["tokens"]["id_token"], "id-token-must-survive");
        assert_eq!(written["tokens"]["access_token"], next_access);
        assert_eq!(written["tokens"]["refresh_token"], "refresh-new");
    }

    #[tokio::test]
    async fn fresh_token_skips_refresh_transport() {
        let home = tempfile::tempdir().unwrap();
        let access = jwt("account-one", future_expiry());
        write_auth(&home, &auth_document(&access, "refresh-current")).await;
        let refresher =
            CodexOAuthRefresher::with_token_url(reqwest::Client::new(), "http://127.0.0.1:9/never");
        let resolved = bridge(home.path())
            .resolve_oauth(&refresher)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.access, access);
        assert_eq!(resolved.refresh, "refresh-current");
    }

    #[tokio::test]
    async fn refresh_failure_keeps_original_file_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let original = auth_document(&jwt("account-one", 1), "refresh-old");
        write_auth(&home, &original).await;
        let server = TokenServer::start(400, json!({"error":"reused"})).await;
        let refresher =
            CodexOAuthRefresher::with_token_url(reqwest::Client::new(), server.url.clone());
        let error = bridge(home.path())
            .resolve_oauth(&refresher)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("OpenAI Codex token refresh failed (400)")
        );
        let after: Value = serde_json::from_slice(
            &tokio::fs::read(home.path().join("auth.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(after, original);
    }
}
