//! Frozen launch-environment layering and Node-compatible dotenv parsing.

use std::{
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::Arc,
};

use indexmap::IndexMap;
use path_clean::PathClean as _;
use seekdeep_util::{
    home_paths::{SEEKDEEP_HOME_ENV, resolve_seekdeep_home},
    launch_environment::{
        LaunchEnvironmentLayerInput, LaunchEnvironmentSnapshot, LaunchEnvironmentSource,
        create_launch_environment_snapshot,
    },
};

/// Owned environment passed to the Rust runtime instead of mutating the
/// process-global environment after threads may exist.
pub type EnvironmentMap = BTreeMap<String, String>;

/// One-line diagnostic sink.
pub type EnvironmentWarning = Arc<dyn Fn(String) + Send + Sync>;

const BOOTSTRAP_NAMES: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "SHELL",
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_EXTRA_CA_CERTS",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "BASH_ENV",
    "ENV",
    "SHELLOPTS",
    "BASHOPTS",
    "PERL5OPT",
    "PERL5LIB",
    "PYTHONSTARTUP",
    "PYTHONPATH",
    "RUBYOPT",
    "RUBYLIB",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "PYTHONHOME",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "DEEPSEEK_BASE_URL",
    "DEEPSEEK_SEARCH_BASE_URL",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "NODE_TLS_REJECT_UNAUTHORIZED",
];

const BOOTSTRAP_PREFIXES: &[&str] = &["SEEKDEEP_", "DSH_", "XDG_", "DYLD_", "BASH_FUNC_"];

#[derive(Debug)]
struct EnvironmentLayer {
    path: PathBuf,
    values: EnvironmentMap,
}

/// Parses the dotenv dialect used by Node's `util.parseEnv`.
///
/// Carriage returns are discarded, duplicate keys are last-wins, `export ` is
/// removed from a key, all three quote forms can span lines, and only literal
/// `\\n` sequences inside double quotes expand to newlines.
#[must_use]
pub fn parse_env(input: &str) -> EnvironmentMap {
    let storage = input.replace('\r', "");
    let mut content = trim_spaces(&storage);
    let mut values = IndexMap::<String, String>::new();

    while !content.is_empty() {
        if matches!(content.as_bytes().first(), Some(b'\n' | b'#')) {
            content = content
                .find('\n')
                .map_or("", |newline| &content[newline + 1..]);
            continue;
        }

        let equals = content.find('=');
        let newline = content.find('\n');
        let separator = match (equals, newline) {
            (Some(equals), Some(newline)) if newline < equals => {
                content = trim_spaces(&content[newline + 1..]);
                continue;
            }
            (Some(equals), _) => equals,
            (None, Some(newline)) => {
                content = trim_spaces(&content[newline + 1..]);
                continue;
            }
            (None, None) => break,
        };

        let mut key = trim_spaces(&content[..separator]);
        content = &content[separator + 1..];
        if content.is_empty() || content.starts_with('\n') {
            values.insert(key.to_owned(), String::new());
            continue;
        }

        content = trim_spaces(content);
        if key.is_empty() {
            continue;
        }
        if let Some(unprefixed) = key.strip_prefix("export ") {
            key = trim_spaces(unprefixed);
        }
        if content.is_empty() {
            values.insert(key.to_owned(), String::new());
            break;
        }

        if content.starts_with('"')
            && let Some(closing_quote) = content[1..].find('"').map(|index| index + 1)
        {
            values.insert(
                key.to_owned(),
                content[1..closing_quote].replace("\\n", "\n"),
            );
            content = content[closing_quote + 1..]
                .find('\n')
                .map_or("", |newline| &content[closing_quote + 2 + newline..]);
            content = trim_spaces(content);
            continue;
        }

        if matches!(content.as_bytes().first(), Some(b'\'' | b'"' | b'`')) {
            let quote = content.as_bytes()[0];
            if let Some(closing_quote) = content.as_bytes()[1..]
                .iter()
                .position(|byte| *byte == quote)
                .map(|index| index + 1)
            {
                values.insert(key.to_owned(), content[1..closing_quote].to_owned());
                content = content[closing_quote + 1..]
                    .find('\n')
                    .map_or("", |newline| &content[closing_quote + 2 + newline..]);
            } else if let Some(newline) = content.find('\n') {
                values.insert(key.to_owned(), content[..newline].to_owned());
                content = &content[newline + 1..];
            } else {
                values.insert(key.to_owned(), content.to_owned());
                break;
            }
        } else if let Some(newline) = content.find('\n') {
            let value = content[..newline]
                .split_once('#')
                .map_or(&content[..newline], |(value, _)| value);
            values.insert(key.to_owned(), trim_spaces(value).to_owned());
            content = &content[newline + 1..];
        } else {
            let value = content.split_once('#').map_or(content, |(value, _)| value);
            values.insert(key.to_owned(), trim_spaces(value).to_owned());
            content = "";
        }
        content = trim_spaces(content);
    }

    values.into_iter().collect()
}

/// Loads one optional `.env` into an owned runtime environment without
/// replacing inherited values.
///
pub fn load_env(
    bin_name: &str,
    directory: &Path,
    environment: &mut EnvironmentMap,
    warn: Option<&EnvironmentWarning>,
) {
    let path = resolve_from(directory, Path::new(".env"));
    let Some(layer) = read_env_file(bin_name, path, warn) else {
        return;
    };
    for (name, value) in layer.values {
        environment.entry(name).or_insert(value);
    }
}

/// Loads inherited > project `.env` > SeekDeep-home `.env` as one immutable
/// runtime generation.
///
/// The home is resolved from the inherited environment before either file is
/// parsed. Both files are parsed and bootstrap-fenced before the resulting
/// values are materialized, so a rejected lower layer cannot partially alter
/// the launch generation.
///
/// # Errors
///
/// Returns path-resolution failures or a bootstrap-only variable declared by
/// either dotenv layer.
pub fn load_layered_env(
    bin_name: &str,
    cwd: &Path,
    inherited: &EnvironmentMap,
    warn: Option<&EnvironmentWarning>,
) -> anyhow::Result<LaunchEnvironmentSnapshot> {
    let cwd = resolve_from(&std::env::current_dir()?, cwd);
    let configured_home = environment_value(inherited, SEEKDEEP_HOME_ENV);
    let home = resolve_seekdeep_home(
        configured_home.map(OsStr::new),
        &HashMap::<OsString, OsString>::new(),
    )?;

    let project = read_env_layer(bin_name, cwd.join(".env"), warn)?;
    let user = if home == cwd {
        None
    } else {
        read_env_layer(bin_name, home.join(".env"), warn)?
    };

    let mut layers = vec![LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: inherited.clone(),
    }];
    if let Some(project) = project {
        layers.push(LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::ProjectEnv,
            path: Some(project.path),
            values: project.values,
        });
    }
    if let Some(user) = user {
        layers.push(LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::UserEnv,
            path: Some(user.path),
            values: user.values,
        });
    }
    Ok(create_launch_environment_snapshot(&layers))
}

fn read_env_layer(
    bin_name: &str,
    path: PathBuf,
    warn: Option<&EnvironmentWarning>,
) -> anyhow::Result<Option<EnvironmentLayer>> {
    let Some(layer) = read_env_file(bin_name, path, warn) else {
        return Ok(None);
    };
    if let Some(name) = layer.values.keys().find(|name| is_bootstrap_only(name)) {
        anyhow::bail!(
            "{bin_name}: {} sets \"{name}\", which only the launching environment may set (it decides how this process starts, where its code and instructions load from, or how it reaches the network); export {name} instead of putting it in a .env file",
            layer.path.display()
        );
    }
    Ok(Some(layer))
}

fn read_env_file(
    bin_name: &str,
    path: PathBuf,
    warn: Option<&EnvironmentWarning>,
) -> Option<EnvironmentLayer> {
    match std::fs::read_to_string(&path) {
        Ok(content) => Some(EnvironmentLayer {
            path,
            values: parse_env(&content),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            let line = format!("{bin_name}: failed to load .env: {error}\n");
            if let Some(warn) = warn {
                warn(line);
            } else {
                eprint!("{line}");
            }
            None
        }
    }
}

fn is_bootstrap_only(name: &str) -> bool {
    let upper = name.to_uppercase();
    BOOTSTRAP_NAMES.contains(&upper.as_str())
        || BOOTSTRAP_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

fn environment_value<'a>(environment: &'a EnvironmentMap, name: &str) -> Option<&'a str> {
    if cfg!(windows) {
        environment
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    } else {
        environment.get(name).map(String::as_str)
    }
}

fn resolve_from(base: &Path, path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    path.clean()
}

fn trim_spaces(value: &str) -> &str {
    value.trim_matches([' ', '\t', '\n'])
}
