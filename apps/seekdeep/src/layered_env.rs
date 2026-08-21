//! Launcher-owned `.env` discovery and immutable launch-environment assembly.

use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    ffi::OsString,
    fmt, fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use seekdeep_util::{
    home_paths::{HomePathError, resolve_seekdeep_home},
    launch_environment::{
        LaunchEnvironmentLayerInput, LaunchEnvironmentSnapshot, LaunchEnvironmentSource,
        create_launch_environment_snapshot,
    },
};

const BOOTSTRAP_NAMES: &[&str] = &[
    // Process launch and module resolution.
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
    // Interpreter startup hooks.
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
    // Version-control command hooks and config redirects.
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
    // Network reach and trust.
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

const BOOTSTRAP_PREFIXES: &[&str] = &["SEEKDEEP_", "XDG_", "DYLD_", "BASH_FUNC_"];

/// The launcher's resolved Harness home and frozen environment layers.
#[derive(Clone, Debug)]
pub struct LayeredEnvironment {
    /// Home selected from the inherited environment before either file is read.
    pub seekdeep_home: PathBuf,
    /// Inherited, project, and user layers with their original provenance.
    pub launch_environment: LaunchEnvironmentSnapshot,
}

/// A failure that prevents construction of the launch environment.
#[derive(Debug)]
pub enum LoadLayeredEnvError {
    /// Harness-home resolution failed before file discovery.
    Home(HomePathError),
    /// The invoking directory could not be made absolute.
    CurrentDirectory(io::Error),
    /// A discovered file attempted to change process bootstrap.
    BootstrapOnly {
        /// Prefix supplied by the executable for launcher diagnostics.
        bin_name: String,
        /// Absolute `.env` path that declared the rejected name.
        path: PathBuf,
        /// Variable name exactly as declared in the file.
        name: String,
    },
}

impl fmt::Display for LoadLayeredEnvError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Home(error) => fmt::Display::fmt(error, formatter),
            Self::CurrentDirectory(error) => {
                write!(
                    formatter,
                    "failed to resolve the invoking directory: {error}"
                )
            }
            Self::BootstrapOnly {
                bin_name,
                path,
                name,
            } => write!(
                formatter,
                "{bin_name}: {} sets \"{name}\", which only the launching environment may set \
                 (it decides how this process starts, where its code and instructions load from, \
                 or how it reaches the network); export {name} instead of putting it in a .env file",
                path.display()
            ),
        }
    }
}

impl Error for LoadLayeredEnvError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Home(error) => Some(error),
            Self::CurrentDirectory(error) => Some(error),
            Self::BootstrapOnly { .. } => None,
        }
    }
}

impl From<HomePathError> for LoadLayeredEnvError {
    fn from(error: HomePathError) -> Self {
        Self::Home(error)
    }
}

#[derive(Debug)]
struct EnvironmentLayer {
    path: PathBuf,
    values: BTreeMap<String, String>,
}

/// Loads the inherited > invoking-directory `.env` > Harness-home `.env`
/// snapshot without modifying the process environment.
///
/// The Harness home and inherited layer are frozen before file discovery. A
/// missing file contributes no layer; another read failure emits one warning
/// and contributes no layer. Both accepted files retain their full contents so
/// consumers can select trusted sources with [`LaunchEnvironmentSnapshot`].
///
/// # Errors
///
/// Returns an error when the home or invoking directory cannot be resolved, or
/// when either file declares a bootstrap-only variable.
pub fn load_layered_env(
    bin_name: &str,
    cwd: &Path,
) -> Result<LayeredEnvironment, LoadLayeredEnvError> {
    let inherited = std::env::vars_os().collect::<HashMap<_, _>>();
    let mut stderr = io::stderr().lock();
    load_layered_env_from(bin_name, cwd, &inherited, |line| {
        let _ = stderr.write_all(line.as_bytes());
    })
}

/// Testable form of [`load_layered_env`] over an explicit inherited mapping and
/// warning sink.
///
/// # Errors
///
/// Returns the same failures as [`load_layered_env`].
pub fn load_layered_env_from<S, F>(
    bin_name: &str,
    cwd: &Path,
    inherited: &HashMap<OsString, OsString, S>,
    mut warn: F,
) -> Result<LayeredEnvironment, LoadLayeredEnvError>
where
    S: std::hash::BuildHasher,
    F: FnMut(&str),
{
    // Home selection is a bootstrap decision: no discovered file may redirect
    // which user layer is read.
    let seekdeep_home = resolve_seekdeep_home(None, inherited)?;
    let project_directory = absolute_lexical(cwd)?;
    let inherited_values = inherited
        .iter()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    // Parse and validate every contributing layer before constructing a result.
    let project = read_env_layer(bin_name, &project_directory, &mut warn)?;
    let user = if seekdeep_home == project_directory {
        None
    } else {
        read_env_layer(bin_name, &seekdeep_home, &mut warn)?
    };

    let mut layers = vec![LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: inherited_values,
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

    Ok(LayeredEnvironment {
        seekdeep_home,
        launch_environment: create_launch_environment_snapshot(&layers),
    })
}

fn read_env_layer<F>(
    bin_name: &str,
    directory: &Path,
    warn: &mut F,
) -> Result<Option<EnvironmentLayer>, LoadLayeredEnvError>
where
    F: FnMut(&str),
{
    let path = directory.join(".env");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            warn(&format!("{bin_name}: failed to load .env: {error}\n"));
            return Ok(None);
        }
    };
    // Node's UTF-8 file reads replace malformed byte sequences rather than
    // rejecting the entire layer.
    let values = parse_node_env(&String::from_utf8_lossy(&bytes));
    for name in values.keys() {
        if !is_bootstrap_only(name) {
            continue;
        }
        return Err(LoadLayeredEnvError::BootstrapOnly {
            bin_name: bin_name.to_owned(),
            path,
            name: name.clone(),
        });
    }
    Ok(Some(EnvironmentLayer { path, values }))
}

fn is_bootstrap_only(name: &str) -> bool {
    let upper = name.to_uppercase();
    BOOTSTRAP_NAMES.contains(&upper.as_str())
        || BOOTSTRAP_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

fn absolute_lexical(path: &Path) -> Result<PathBuf, LoadLayeredEnvError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(LoadLayeredEnvError::CurrentDirectory)?
            .join(path)
    };
    Ok(clean_absolute_path(&absolute))
}

fn clean_absolute_path(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => clean.push(prefix.as_os_str()),
            Component::RootDir => clean.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = clean.pop();
            }
            Component::Normal(segment) => clean.push(segment),
        }
    }
    clean
}

// This is the parser used by Node 22.19 and 24.9 `util.parseEnv`, whose
// permissive details are observable through the pinned source loader. It does
// not expand references or escapes other than literal `\\n` in double quotes.
fn parse_node_env(input: &str) -> BTreeMap<String, String> {
    let normalized = input.replace('\r', "");
    let mut content = trim_node_spaces(&normalized);
    let mut values = BTreeMap::new();

    while !content.is_empty() {
        if content.starts_with('\n') || content.starts_with('#') {
            content = match content.find('\n') {
                Some(newline) => &content[newline + 1..],
                None => "",
            };
            continue;
        }

        let Some(equal_or_newline) = content.find(['=', '\n']) else {
            break;
        };
        if content.as_bytes()[equal_or_newline] == b'\n' {
            content = trim_node_spaces(&content[equal_or_newline + 1..]);
            continue;
        }

        let mut key = trim_node_spaces(&content[..equal_or_newline]);
        content = &content[equal_or_newline + 1..];
        if content.is_empty() || content.starts_with('\n') {
            insert_node_value(&mut values, key, String::new());
            continue;
        }

        content = trim_node_spaces(content);
        if key.is_empty() {
            continue;
        }
        if let Some(unprefixed) = key.strip_prefix("export ") {
            key = trim_node_spaces(unprefixed);
        }
        if content.is_empty() {
            insert_node_value(&mut values, key, String::new());
            break;
        }

        if content.starts_with('"')
            && let Some(closing_quote) = find_closing_quote(content, '"')
        {
            let value = content[1..closing_quote].replace("\\n", "\n");
            insert_node_value(&mut values, key, value);
            content = after_quoted_line(content, closing_quote);
            continue;
        }

        if let Some(quote) = content
            .chars()
            .next()
            .filter(|quote| matches!(quote, '\'' | '"' | '`'))
        {
            if let Some(closing_quote) = find_closing_quote(content, quote) {
                insert_node_value(&mut values, key, content[1..closing_quote].to_owned());
                content = after_quoted_line(content, closing_quote);
                continue;
            }
            if let Some(newline) = content.find('\n') {
                insert_node_value(&mut values, key, content[..newline].to_owned());
                content = &content[newline + 1..];
            } else {
                insert_node_value(&mut values, key, content.to_owned());
                break;
            }
        } else if let Some(newline) = content.find('\n') {
            let line = &content[..newline];
            let value = line.split_once('#').map_or(line, |(value, _)| value);
            insert_node_value(&mut values, key, trim_node_spaces(value).to_owned());
            content = &content[newline + 1..];
        } else {
            let value = content.split_once('#').map_or(content, |(value, _)| value);
            insert_node_value(&mut values, key, trim_node_spaces(value).to_owned());
            content = "";
        }
        content = trim_node_spaces(content);
    }

    values
}

fn insert_node_value(values: &mut BTreeMap<String, String>, key: &str, value: String) {
    // `util.parseEnv` returns an ordinary JavaScript object. Setting this exact
    // legacy accessor name changes no own property, so `Object.entries` drops it.
    if key != "__proto__" {
        values.insert(key.to_owned(), value);
    }
}

fn trim_node_spaces(value: &str) -> &str {
    value.trim_matches([' ', '\t', '\n'])
}

fn find_closing_quote(content: &str, quote: char) -> Option<usize> {
    content[1..].find(quote).map(|offset| offset + 1)
}

fn after_quoted_line(content: &str, closing_quote: usize) -> &str {
    content[closing_quote + 1..]
        .find('\n')
        .map_or("", |newline| &content[closing_quote + newline + 2..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use seekdeep_util::launch_environment::{LaunchEnvironmentEntry, LaunchEnvironmentSource};
    use tempfile::TempDir;

    const BIN: &str = "seekdeep-test-bin";

    fn environment(entries: &[(&str, &Path)]) -> HashMap<OsString, OsString> {
        entries
            .iter()
            .map(|(name, value)| (OsString::from(name), value.as_os_str().to_owned()))
            .collect()
    }

    fn write_env(directory: &Path, content: &str) {
        fs::write(directory.join(".env"), content).unwrap();
    }

    #[test]
    fn parser_matches_node_quotes_comments_exports_duplicates_and_crlf() {
        let parsed = parse_node_env(
            " A = plain # tail\n\
             B=\"double\\nline # kept\" junk\n\
             C=' single # kept ' ignored\n\
             D=`tick#kept` x\n\
             export   E = exported\n\
             A=last\r\n",
        );
        assert_eq!(parsed.get("A").map(String::as_str), Some("last"));
        assert_eq!(
            parsed.get("B").map(String::as_str),
            Some("double\nline # kept")
        );
        assert_eq!(parsed.get("C").map(String::as_str), Some(" single # kept "));
        assert_eq!(parsed.get("D").map(String::as_str), Some("tick#kept"));
        assert_eq!(parsed.get("E").map(String::as_str), Some("exported"));
    }

    #[test]
    fn parser_preserves_node_permissive_and_edge_case_behavior() {
        assert_eq!(
            parse_node_env("NO_EQUALS\n=value\n9BAD=nine\nDASH-KEY=works\nexport F=\n"),
            BTreeMap::from([
                ("9BAD".to_owned(), "nine".to_owned()),
                ("DASH-KEY".to_owned(), "works".to_owned()),
                ("export F".to_owned(), String::new()),
            ])
        );
        assert_eq!(
            parse_node_env("A=\nB=   \nC=x\n"),
            BTreeMap::from([
                ("A".to_owned(), String::new()),
                ("B".to_owned(), "C=x".to_owned()),
            ])
        );
        assert_eq!(
            parse_node_env("A=\"unterminated\nB=next\nC='rest"),
            BTreeMap::from([
                ("A".to_owned(), "\"unterminated".to_owned()),
                ("B".to_owned(), "next".to_owned()),
                ("C".to_owned(), "'rest".to_owned()),
            ])
        );
        assert_eq!(
            parse_node_env("__proto__=dropped\nconstructor=kept\n"),
            BTreeMap::from([("constructor".to_owned(), "kept".to_owned())])
        );
    }

    #[test]
    fn layers_user_under_project_under_inherited_with_exact_provenance() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        write_env(
            home.path(),
            "SHARED=user\nUSER_ONLY=user-only\nINHERITED=user-loses\n",
        );
        write_env(
            project.path(),
            "SHARED=project\nPROJECT_ONLY=project-only\nINHERITED=project-loses\n",
        );
        let mut inherited = environment(&[("SEEKDEEP_HOME", home.path())]);
        inherited.insert("INHERITED".into(), "inherited".into());
        let loaded = load_layered_env_from(BIN, project.path(), &inherited, |_| {}).unwrap();

        assert_eq!(loaded.seekdeep_home, home.path());
        assert_eq!(
            loaded.launch_environment.get("SHARED"),
            Some(LaunchEnvironmentEntry {
                value: "project".into(),
                source: LaunchEnvironmentSource::ProjectEnv,
                path: Some(project.path().join(".env")),
            })
        );
        assert_eq!(
            loaded.launch_environment.get("USER_ONLY"),
            Some(LaunchEnvironmentEntry {
                value: "user-only".into(),
                source: LaunchEnvironmentSource::UserEnv,
                path: Some(home.path().join(".env")),
            })
        );
        assert_eq!(
            loaded.launch_environment.get("INHERITED"),
            Some(LaunchEnvironmentEntry {
                value: "inherited".into(),
                source: LaunchEnvironmentSource::Process,
                path: None,
            })
        );
    }

    #[test]
    fn bootstrap_names_are_case_insensitive_and_rejection_is_atomic() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        write_env(project.path(), "WOULD_BE_ACCEPTED=project\n");
        write_env(home.path(), "https_proxy=http://attacker.example\n");
        let inherited = environment(&[("SEEKDEEP_HOME", home.path())]);
        let before = std::env::var_os("WOULD_BE_ACCEPTED");
        let error = load_layered_env_from(BIN, project.path(), &inherited, |_| {})
            .expect_err("the user layer must reject the whole load");
        assert_eq!(
            error.to_string(),
            format!(
                "{BIN}: {} sets \"https_proxy\", which only the launching environment may set \
                 (it decides how this process starts, where its code and instructions load from, \
                 or how it reaches the network); export https_proxy instead of putting it in a .env file",
                home.path().join(".env").display()
            )
        );
        assert_eq!(std::env::var_os("WOULD_BE_ACCEPTED"), before);
    }

    #[test]
    fn product_bootstrap_prefix_is_renamed_and_home_cannot_come_from_a_file() {
        let home = TempDir::new().unwrap();
        let redirected = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        write_env(home.path(), "FROM_REAL_HOME=yes\n");
        write_env(
            project.path(),
            &format!("SEEKDEEP_HOME={}\n", redirected.path().display()),
        );
        let inherited = environment(&[("SEEKDEEP_HOME", home.path())]);
        let error = load_layered_env_from(BIN, project.path(), &inherited, |_| {})
            .expect_err("a file must not redirect the Harness home");
        assert_eq!(
            error.to_string(),
            format!(
                "{BIN}: {} sets \"SEEKDEEP_HOME\", which only the launching environment may set \
                 (it decides how this process starts, where its code and instructions load from, \
                 or how it reaches the network); export SEEKDEEP_HOME instead of putting it in a .env file",
                project.path().join(".env").display()
            )
        );
    }

    #[test]
    fn absent_layers_are_silent_and_unreadable_layers_warn_once() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        fs::create_dir(home.path().join(".env")).unwrap();
        write_env(project.path(), "PROJECT_ONLY=yes\n");
        let inherited = environment(&[("SEEKDEEP_HOME", home.path())]);
        let mut warnings = Vec::new();
        let loaded = load_layered_env_from(BIN, project.path(), &inherited, |line| {
            warnings.push(line.to_owned());
        })
        .unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with(&format!("{BIN}: failed to load .env: ")));
        assert!(warnings[0].ends_with('\n'));
        assert_eq!(
            loaded
                .launch_environment
                .get("PROJECT_ONLY")
                .unwrap()
                .source,
            LaunchEnvironmentSource::ProjectEnv
        );

        let empty_home = TempDir::new().unwrap();
        let empty_project = TempDir::new().unwrap();
        let inherited = environment(&[("SEEKDEEP_HOME", empty_home.path())]);
        warnings.clear();
        load_layered_env_from(BIN, empty_project.path(), &inherited, |line| {
            warnings.push(line.to_owned());
        })
        .unwrap();
        assert!(warnings.is_empty());
    }

    #[test]
    fn one_file_is_project_layer_when_home_equals_invocation_directory() {
        let both = TempDir::new().unwrap();
        write_env(both.path(), "ONE_FILE=yes\n");
        let inherited = environment(&[("SEEKDEEP_HOME", both.path())]);
        let loaded = load_layered_env_from(BIN, both.path(), &inherited, |_| {}).unwrap();
        assert_eq!(
            loaded.launch_environment.get("ONE_FILE"),
            Some(LaunchEnvironmentEntry {
                value: "yes".into(),
                source: LaunchEnvironmentSource::ProjectEnv,
                path: Some(both.path().join(".env")),
            })
        );
        assert_eq!(
            loaded
                .launch_environment
                .get_from("ONE_FILE", &[LaunchEnvironmentSource::UserEnv]),
            None
        );
    }

    #[test]
    fn inherited_bootstrap_names_are_allowed() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let mut inherited = environment(&[("SEEKDEEP_HOME", home.path())]);
        inherited.insert(
            "HTTPS_PROXY".into(),
            OsString::from("http://trusted.example"),
        );
        let loaded = load_layered_env_from(BIN, project.path(), &inherited, |_| {}).unwrap();
        assert_eq!(
            loaded.launch_environment.get("HTTPS_PROXY").unwrap().source,
            LaunchEnvironmentSource::Process
        );
        assert_eq!(loaded.seekdeep_home, home.path());
    }

    #[test]
    fn relative_invocation_directory_is_resolved_lexically() {
        let current = std::env::current_dir().unwrap();
        let home = TempDir::new().unwrap();
        let inherited = environment(&[("SEEKDEEP_HOME", home.path())]);
        let loaded =
            load_layered_env_from(BIN, Path::new("apps/../apps"), &inherited, |_| {}).unwrap();
        assert_eq!(
            loaded.seekdeep_home,
            home.path(),
            "home remains inherited while cwd resolution is independent"
        );
        assert_eq!(
            clean_absolute_path(&current.join("apps/../apps")),
            current.join("apps")
        );
    }
}
