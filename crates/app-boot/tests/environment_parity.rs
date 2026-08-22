//! Dotenv grammar, layering, provenance, and bootstrap-fence parity.

use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, Mutex},
};

use seekdeep_app_boot::{EnvironmentWarning, load_env, load_layered_env, parse_env};
use seekdeep_util::launch_environment::LaunchEnvironmentSource;

const NAME: &str = "seekdeep-test-bin";

#[test]
fn parser_matches_node_parse_env_quoting_comments_exports_and_duplicates() {
    let parsed = parse_env(concat!(
        "PLAIN = plain value # comment\r\n",
        "export EXPORTED = yes\n",
        "DOUBLE=\"first\\nsecond\" trailing\n",
        "SINGLE='first\nsecond'\n",
        "BACKTICK=`first\nsecond`\n",
        "EMPTY=\n",
        "HASH=# comment\n",
        "DUPLICATE=first\n",
        "DUPLICATE=second\n",
        "INVALID LINE\n",
        "UNCLOSED='rest\n",
    ));
    assert_eq!(parsed["PLAIN"], "plain value");
    assert_eq!(parsed["EXPORTED"], "yes");
    assert_eq!(parsed["DOUBLE"], "first\nsecond");
    assert_eq!(parsed["SINGLE"], "first\nsecond");
    assert_eq!(parsed["BACKTICK"], "first\nsecond");
    assert_eq!(parsed["EMPTY"], "");
    assert_eq!(parsed["HASH"], "");
    assert_eq!(parsed["DUPLICATE"], "second");
    assert_eq!(parsed["UNCLOSED"], "'rest");
    assert!(!parsed.contains_key("INVALID LINE"));
}

#[test]
fn one_file_load_is_optional_warns_once_and_never_replaces_inherited_values() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(
        temporary.path().join(".env"),
        "INHERITED=loses\nFROM_FILE=loaded\n",
    )
    .unwrap();
    let mut environment = BTreeMap::from([("INHERITED".to_owned(), "wins".to_owned())]);
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let warn: EnvironmentWarning = Arc::new({
        let warnings = warnings.clone();
        move |line| warnings.lock().unwrap().push(line)
    });
    load_env(NAME, temporary.path(), &mut environment, Some(&warn));
    assert_eq!(environment["INHERITED"], "wins");
    assert_eq!(environment["FROM_FILE"], "loaded");
    assert!(warnings.lock().unwrap().is_empty());

    load_env(
        NAME,
        &temporary.path().join("absent"),
        &mut environment,
        Some(&warn),
    );
    assert!(warnings.lock().unwrap().is_empty());

    let broken = tempfile::tempdir().unwrap();
    fs::create_dir(broken.path().join(".env")).unwrap();
    load_env(NAME, broken.path(), &mut environment, Some(&warn));
    let warnings = warnings.lock().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].starts_with("seekdeep-test-bin: failed to load .env: "));
    assert!(warnings[0].ends_with('\n'));
}

#[test]
fn layered_environment_preserves_precedence_provenance_and_single_home_read() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let project = root.path().join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::write(
        home.join(".env"),
        "SHARED=user\nUSER_ONLY=user-only\nINHERITED=user-loses\n",
    )
    .unwrap();
    fs::write(
        project.join(".env"),
        "SHARED=project\nPROJECT_ONLY=project-only\nINHERITED=project-loses\n",
    )
    .unwrap();
    let inherited = BTreeMap::from([
        (
            "SEEKDEEP_HOME".to_owned(),
            home.to_string_lossy().into_owned(),
        ),
        ("INHERITED".to_owned(), "inherited".to_owned()),
    ]);
    let snapshot = load_layered_env(NAME, &project, &inherited, None).unwrap();
    assert_eq!(snapshot.get("SHARED").unwrap().value, "project");
    assert_eq!(snapshot.get("USER_ONLY").unwrap().value, "user-only");
    assert_eq!(snapshot.get("PROJECT_ONLY").unwrap().value, "project-only");
    assert_eq!(snapshot.get("INHERITED").unwrap().value, "inherited");
    assert_eq!(
        snapshot.get("USER_ONLY").unwrap().source,
        LaunchEnvironmentSource::UserEnv
    );
    assert_eq!(
        snapshot.get("USER_ONLY").unwrap().path.as_deref(),
        Some(home.join(".env").as_path())
    );
    assert_eq!(
        snapshot.get("PROJECT_ONLY").unwrap().source,
        LaunchEnvironmentSource::ProjectEnv
    );
    assert!(
        snapshot
            .get_from(
                "PROJECT_ONLY",
                &[
                    LaunchEnvironmentSource::Process,
                    LaunchEnvironmentSource::UserEnv,
                ],
            )
            .is_none()
    );

    let same = root.path().join("same");
    fs::create_dir(&same).unwrap();
    fs::write(same.join(".env"), "ONE_FILE=value\n").unwrap();
    let inherited = BTreeMap::from([(
        "SEEKDEEP_HOME".to_owned(),
        same.to_string_lossy().into_owned(),
    )]);
    let snapshot = load_layered_env(NAME, &same, &inherited, None).unwrap();
    let entry = snapshot.get("ONE_FILE").unwrap();
    assert_eq!(entry.source, LaunchEnvironmentSource::ProjectEnv);
    assert_eq!(entry.path.as_deref(), Some(same.join(".env").as_path()));
}

#[test]
fn bootstrap_only_names_reject_both_layers_before_a_generation_is_returned() {
    for declaration in [
        "SEEKDEEP_PERMISSION_MODE=danger-full-access\n",
        "DSH_PERMISSION_MODE=danger-full-access\n",
        "PATH=/tmp/evil\n",
        "NODE_OPTIONS=--require /tmp/evil.js\n",
        "HTTPS_PROXY=http://attacker.example\n",
        "https_proxy=http://attacker.example\n",
        "XDG_CONFIG_HOME=/tmp/evil\n",
    ] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let project = root.path().join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join(".env"),
            format!("WOULD_HAVE_APPLIED=yes\n{declaration}"),
        )
        .unwrap();
        let inherited = BTreeMap::from([(
            "SEEKDEEP_HOME".to_owned(),
            home.to_string_lossy().into_owned(),
        )]);
        let error = load_layered_env(NAME, &project, &inherited, None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only the launching environment may set"),
            "{declaration:?}: {error}"
        );
    }
}

#[test]
fn unreadable_and_absent_layers_are_nonfatal_and_home_comes_only_from_inherited() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let project = root.path().join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::create_dir(home.join(".env")).unwrap();
    fs::write(project.join(".env"), "PROJECT_ONLY=yes\n").unwrap();
    let inherited = BTreeMap::from([(
        "SEEKDEEP_HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    )]);
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let warn: EnvironmentWarning = Arc::new({
        let warnings = warnings.clone();
        move |line| warnings.lock().unwrap().push(line)
    });
    let snapshot = load_layered_env(NAME, &project, &inherited, Some(&warn)).unwrap();
    assert_eq!(snapshot.get("PROJECT_ONLY").unwrap().value, "yes");
    assert_eq!(warnings.lock().unwrap().len(), 1);

    fs::remove_dir(home.join(".env")).unwrap();
    warnings.lock().unwrap().clear();
    let snapshot = load_layered_env(NAME, &project, &inherited, Some(&warn)).unwrap();
    assert_eq!(snapshot.get("PROJECT_ONLY").unwrap().value, "yes");
    assert!(warnings.lock().unwrap().is_empty());
}
