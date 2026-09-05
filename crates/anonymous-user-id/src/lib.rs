//! Harness-home-scoped anonymous user identity.

use std::{
    collections::HashMap,
    ffi::OsString,
    fs::OpenOptions,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use parking_lot::Mutex;
use seekdeep_util::home_paths::resolve_seekdeep_home;

/// Package-owned invariant companion.
pub mod invariant;

seekdeep_util::string_brand!(
    /// Anonymous identity scoped to one resolved harness home.
    pub struct AnonymousUserId;
);

/// Bare UUID file inside the harness home.
pub const ANONYMOUS_USER_ID_FILE_NAME: &str = ".anonymous-user-id";

type Generator = Arc<dyn Fn() -> String + Send + Sync + 'static>;

/// Environment and UUID-generation seams.
#[derive(Clone, Default)]
pub struct AnonymousUserIdOptions {
    /// Environment consulted for `SEEKDEEP_HOME`.
    pub env: Option<HashMap<OsString, OsString>>,
    /// UUID generator, primarily for deterministic and concurrency tests.
    pub random_uuid: Option<Generator>,
}

impl std::fmt::Debug for AnonymousUserIdOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnonymousUserIdOptions")
            .field("env", &self.env)
            .field(
                "random_uuid",
                &self.random_uuid.as_ref().map(|_| "<generator>"),
            )
            .finish()
    }
}

static MEMO: OnceLock<Mutex<HashMap<PathBuf, AnonymousUserId>>> = OnceLock::new();

/// Returns the stable anonymous id for the selected harness home.
///
/// A missing or corrupt file is replaced using exclusive creation first so a
/// concurrent winner can be adopted. File persistence is best effort; path
/// resolution failures remain actionable errors.
///
/// # Errors
///
/// Returns harness-home resolution failures.
pub fn get_or_create_anonymous_user_id(
    options: AnonymousUserIdOptions,
) -> anyhow::Result<AnonymousUserId> {
    let process_environment;
    let environment = if let Some(environment) = options.env.as_ref() {
        environment
    } else {
        process_environment = std::env::vars_os().collect::<HashMap<_, _>>();
        &process_environment
    };
    let home = resolve_seekdeep_home(None, environment)?;
    let file = home.join(ANONYMOUS_USER_ID_FILE_NAME);
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = memo.lock().get(&file).cloned() {
        return Ok(cached);
    }

    let id = read_persisted_id(&file).unwrap_or_else(|| {
        let created = AnonymousUserId::new(
            options
                .random_uuid
                .map_or_else(|| uuid::Uuid::new_v4().to_string(), |generate| generate()),
        );
        persist_or_adopt(&home, &file, created)
    });
    memo.lock().insert(file, id.clone());
    Ok(id)
}

fn read_persisted_id(file: &Path) -> Option<AnonymousUserId> {
    let text = std::fs::read_to_string(file).ok()?;
    let value = text.trim();
    canonical_uuid_shape(value).then(|| AnonymousUserId::new(value))
}

fn persist_or_adopt(home: &Path, file: &Path, created: AnonymousUserId) -> AnonymousUserId {
    let exclusive = (|| -> anyhow::Result<()> {
        std::fs::create_dir_all(home)?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(file)?;
        writeln!(output, "{created}")?;
        Ok(())
    })();
    if exclusive.is_ok() {
        return created;
    }
    if let Some(winner) = read_persisted_id(file) {
        return winner;
    }
    let _ = std::fs::write(file, format!("{created}\n"));
    created
}

fn canonical_uuid_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}
