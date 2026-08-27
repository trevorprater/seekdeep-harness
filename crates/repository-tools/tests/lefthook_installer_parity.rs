//! Worktree isolation, ownership, migration, locking, rollback, and probe parity.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

use seekdeep_repository_tools::lefthook_installer::{
    InstallerLockTiming, LefthookInstallOptions, install_lefthook,
};

struct Fixture {
    container: tempfile::TempDir,
    environment: BTreeMap<OsString, OsString>,
    main: PathBuf,
    linked: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let container = tempfile::tempdir().unwrap();
        let main = container.path().join("main");
        let linked = container.path().join("linked");
        let mut environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
        environment.insert(
            "GIT_CONFIG_GLOBAL".into(),
            container.path().join("global.gitconfig").into_os_string(),
        );
        environment.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        environment.insert("GIT_DEFAULT_HASH".into(), "sha1".into());
        let fixture = Self {
            container,
            environment,
            main,
            linked,
        };
        fixture.command_success(
            "git",
            &[
                "init",
                "--quiet",
                "--initial-branch=master",
                fixture.main.to_str().unwrap(),
            ],
            fixture.container.path(),
        );
        fixture.git(
            &fixture.main,
            &["config", "user.email", "hooks@example.test"],
        );
        fixture.git(&fixture.main, &["config", "user.name", "Hooks Test"]);
        fixture.write(&fixture.main.join("tracked.txt"), "tracked\n", None);
        fixture.write(
            &fixture.main.join("lefthook.yml"),
            "main-worktree-config\n",
            None,
        );
        fixture.git(&fixture.main, &["add", "."]);
        fixture.git(&fixture.main, &["commit", "--quiet", "-m", "base"]);
        fixture.git_owned(
            &fixture.main,
            &[
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from("--quiet"),
                OsString::from("-b"),
                OsString::from("linked"),
                fixture.linked.as_os_str().to_owned(),
            ],
        );
        fixture.write(
            &fixture.linked.join("lefthook.yml"),
            "linked-worktree-config\n",
            None,
        );
        fixture.install_fake_lefthook(&fixture.main);
        fixture.install_fake_lefthook(&fixture.linked);
        fixture
    }

    fn command(&self, program: &str, args: &[&str], cwd: &Path) -> Output {
        Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(&self.environment)
            .output()
            .unwrap()
    }

    fn command_success(&self, program: &str, args: &[&str], cwd: &Path) -> String {
        let output = self.command(program, args, cwd);
        assert_success(&output);
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn git(&self, root: &Path, args: &[&str]) -> String {
        self.command_success("git", args, root)
    }

    fn git_owned(&self, root: &Path, args: &[OsString]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env_clear()
            .envs(&self.environment)
            .output()
            .unwrap();
        assert_success(&output);
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn git_output(&self, root: &Path, args: &[&str]) -> Output {
        self.command("git", args, root)
    }

    fn write(&self, path: &Path, content: &str, mode: Option<u32>) {
        debug_assert!(self.container.path().is_absolute());
        debug_assert!(path.is_absolute());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
    }

    fn options(&self, root: &Path) -> LefthookInstallOptions {
        LefthookInstallOptions {
            lefthook: root.join("node_modules/.bin/lefthook"),
            pairing_driver: PathBuf::from(env!("CARGO_BIN_EXE_merge-translation-pairing")),
            environment: self.environment.clone(),
            lock_timing: InstallerLockTiming {
                wait_timeout: Duration::from_secs(5),
                initialization_timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(10),
            },
        }
    }

    fn install(&self, root: &Path) -> anyhow::Result<()> {
        install_lefthook(root, &self.options(root))
    }

    fn git_directory(&self, root: &Path) -> PathBuf {
        PathBuf::from(self.git(root, &["rev-parse", "--absolute-git-dir"]))
    }

    fn common_directory(&self) -> PathBuf {
        let path = PathBuf::from(self.git(&self.main, &["rev-parse", "--git-common-dir"]));
        if path.is_absolute() {
            path
        } else {
            PathBuf::from(self.git(&self.main, &["rev-parse", "--show-toplevel"])).join(path)
        }
    }

    fn hooks_path(&self, root: &Path) -> PathBuf {
        self.git_directory(root).join("seekdeep-hooks")
    }

    fn install_lock_path(&self) -> PathBuf {
        self.common_directory()
            .join("seekdeep-lefthook-install.lock")
    }

    fn install_fake_lefthook(&self, root: &Path) {
        let executable = root.join("node_modules/.bin/lefthook");
        self.write(
            &executable,
            r#"#!/bin/sh
if [ "$1 $2" != "install --force" ]; then exit 64; fi
root=$(git rev-parse --show-toplevel) || exit 65
hooks=$(git config --get core.hooksPath) || exit 66
mkdir -p "$hooks" || exit 67
running="$hooks/.fake-lefthook-running"
( set -C; : > "$running" ) 2>/dev/null || exit 91
if [ -n "$SEEKDEEP_TEST_FORBIDDEN_GIT_CONFIG_KEY" ]; then
  if git config --get "$SEEKDEEP_TEST_FORBIDDEN_GIT_CONFIG_KEY" >/dev/null 2>&1; then
    rm -f "$running"
    exit 92
  fi
fi
if [ -n "$SEEKDEEP_TEST_LEFTHOOK_DELAY_SECONDS" ]; then
  sleep "$SEEKDEEP_TEST_LEFTHOOK_DELAY_SECONDS"
fi
if [ "$SEEKDEEP_TEST_LEFTHOOK_FAIL" = "1" ]; then
  rm -f "$running"
  exit 77
fi
config=$(sed -n '1p' "$root/lefthook.yml") || exit 68
for name in pre-commit pre-merge-commit pre-push; do
  printf '#!/bin/sh\n# root=%s\n# config=%s\nexit 0\n' "$root" "$config" > "$hooks/$name" || exit 69
  chmod 755 "$hooks/$name" || exit 70
done
rm -f "$running"
"#,
            Some(0o755),
        );
    }
}

#[test]
fn installs_isolated_hooks_and_pairing_driver_for_main_and_linked_worktrees() {
    let fixture = Fixture::new();
    let common = fixture.common_directory();
    let legacy = common.join("hooks/pre-commit");
    fixture.write(&legacy, "#!/bin/sh\n# legacy\n", Some(0o755));

    fixture.install(&fixture.main).unwrap();
    fixture.install(&fixture.linked).unwrap();

    let main_hooks = fixture.hooks_path(&fixture.main);
    let linked_hooks = fixture.hooks_path(&fixture.linked);
    assert_ne!(main_hooks, linked_hooks);
    assert_eq!(
        fixture.git(
            &fixture.main,
            &["config", "--worktree", "--get", "core.hooksPath"]
        ),
        main_hooks.to_string_lossy()
    );
    assert_eq!(
        fixture.git(
            &fixture.linked,
            &["config", "--worktree", "--get", "core.hooksPath"]
        ),
        linked_hooks.to_string_lossy()
    );
    for root in [&fixture.main, &fixture.linked] {
        assert_eq!(
            fixture.git(
                root,
                &[
                    "config",
                    "--worktree",
                    "--get",
                    "merge.seekdeep-translation-pairing.driver"
                ]
            ),
            "scripts/merge-translation-pairing-driver.sh %O %A %B %P"
        );
        let hook = fs::read_to_string(fixture.hooks_path(root).join("pre-commit")).unwrap();
        let canonical = fixture.git(root, &["rev-parse", "--show-toplevel"]);
        assert!(hook.contains(&format!("# root={canonical}")));
        assert!(fixture.hooks_path(root).join("pre-merge-commit").exists());
    }
    let common_config = common.join("config");
    assert_eq!(
        fixture.git(
            &fixture.main,
            &[
                "config",
                "--file",
                common_config.to_str().unwrap(),
                "--get",
                "core.repositoryFormatVersion"
            ]
        ),
        "1"
    );
    assert_eq!(
        fixture.git(
            &fixture.main,
            &[
                "config",
                "--file",
                common_config.to_str().unwrap(),
                "--get",
                "extensions.worktreeConfig"
            ]
        ),
        "true"
    );
    assert_eq!(fs::read_to_string(legacy).unwrap(), "#!/bin/sh\n# legacy\n");
    assert!(!fixture.install_lock_path().exists());
}

#[test]
fn repeated_and_concurrent_installs_are_serialized_and_stable() {
    let fixture = std::sync::Arc::new(Fixture::new());
    let mut first = fixture.options(&fixture.main);
    first
        .environment
        .insert("SEEKDEEP_TEST_LEFTHOOK_DELAY_SECONDS".into(), "0.15".into());
    let mut second = fixture.options(&fixture.linked);
    second
        .environment
        .insert("SEEKDEEP_TEST_LEFTHOOK_DELAY_SECONDS".into(), "0.15".into());
    let left_root = fixture.main.clone();
    let right_root = fixture.linked.clone();
    let left = std::thread::spawn(move || install_lefthook(&left_root, &first));
    let right = std::thread::spawn(move || install_lefthook(&right_root, &second));
    left.join().unwrap().unwrap();
    right.join().unwrap().unwrap();

    let hook = fixture.hooks_path(&fixture.main).join("pre-push");
    let before = fs::read_to_string(&hook).unwrap();
    fixture.install(&fixture.main).unwrap();
    assert_eq!(fs::read_to_string(hook).unwrap(), before);
    assert!(!fixture.install_lock_path().exists());
}

#[test]
fn stale_and_invalid_locks_require_explicit_recovery() {
    let fixture = Fixture::new();
    let lock = fixture.install_lock_path();
    let completed = Command::new("true").spawn().unwrap();
    let owner = completed.id();
    let mut completed = completed;
    assert!(completed.wait().unwrap().success());
    let stale = format!("{owner} 00000000-0000-4000-8000-000000000000\n");
    fs::write(&lock, &stale).unwrap();
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("stale Lefthook installer lock"));
    assert!(error.contains("remove it manually"));
    assert_eq!(fs::read_to_string(&lock).unwrap(), stale);

    fs::write(&lock, "not an installer lock\n").unwrap();
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("invalid Lefthook installer lock"));
    assert_eq!(
        fs::read_to_string(&lock).unwrap(),
        "not an installer lock\n"
    );
}

#[test]
fn inherited_hook_path_requires_explicit_override_but_worktree_scope_never_does() {
    let fixture = Fixture::new();
    fixture.git(
        &fixture.main,
        &["config", "--local", "core.hooksPath", "custom-hooks"],
    );
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("refusing to replace user-owned core.hooksPath"));
    assert!(error.contains("SEEKDEEP_LEFTHOOK_ALLOW_HOOKS_PATH_OVERRIDE=1"));

    let mut options = fixture.options(&fixture.main);
    options.environment.insert(
        "SEEKDEEP_LEFTHOOK_ALLOW_HOOKS_PATH_OVERRIDE".into(),
        "1".into(),
    );
    install_lefthook(&fixture.main, &options).unwrap();
    assert_eq!(
        fixture.git(&fixture.main, &["config", "--get", "core.hooksPath"]),
        fixture.hooks_path(&fixture.main).to_string_lossy()
    );
    assert_eq!(
        fixture.git(&fixture.linked, &["config", "--get", "core.hooksPath"]),
        "custom-hooks"
    );

    fixture.git(
        &fixture.linked,
        &[
            "config",
            "--worktree",
            "core.hooksPath",
            "linked-custom-hooks",
        ],
    );
    let mut linked_options = fixture.options(&fixture.linked);
    linked_options.environment.insert(
        "SEEKDEEP_LEFTHOOK_ALLOW_HOOKS_PATH_OVERRIDE".into(),
        "1".into(),
    );
    let error = install_lefthook(&fixture.linked, &linked_options)
        .unwrap_err()
        .to_string();
    assert!(error.contains("worktree-scoped core.hooksPath"));
}

#[test]
fn command_scoped_hooks_and_custom_pairing_drivers_are_never_replaced() {
    let fixture = Fixture::new();
    let mut options = fixture.options(&fixture.main);
    options
        .environment
        .insert("GIT_CONFIG_COUNT".into(), "1".into());
    options
        .environment
        .insert("GIT_CONFIG_KEY_0".into(), "core.hooksPath".into());
    options
        .environment
        .insert("GIT_CONFIG_VALUE_0".into(), "/command-hooks".into());
    let error = install_lefthook(&fixture.main, &options)
        .unwrap_err()
        .to_string();
    assert!(error.contains("command-scoped core.hooksPath"));

    let fixture = Fixture::new();
    fixture.git(
        &fixture.main,
        &[
            "config",
            "--local",
            "merge.seekdeep-translation-pairing.driver",
            "custom-driver %A",
        ],
    );
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("refusing to mask inherited merge.seekdeep-translation-pairing.driver"));
}

#[test]
fn lefthook_failure_and_probe_failure_publish_no_worktree_integration() {
    let fixture = Fixture::new();
    let mut options = fixture.options(&fixture.main);
    options
        .environment
        .insert("SEEKDEEP_TEST_LEFTHOOK_FAIL".into(), "1".into());
    let error = install_lefthook(&fixture.main, &options)
        .unwrap_err()
        .to_string();
    assert!(error.contains("exit status 77"));
    assert_eq!(
        fixture
            .git_output(
                &fixture.main,
                &["config", "--worktree", "--get", "core.hooksPath"]
            )
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        fixture
            .git_output(
                &fixture.main,
                &[
                    "config",
                    "--worktree",
                    "--get",
                    "merge.seekdeep-translation-pairing.driver"
                ]
            )
            .status
            .code(),
        Some(1)
    );

    let fixture = Fixture::new();
    let mut options = fixture.options(&fixture.main);
    options.pairing_driver = fixture.container.path().join("missing-driver");
    let error = install_lefthook(&fixture.main, &options)
        .unwrap_err()
        .to_string();
    assert!(error.contains("missing-driver --probe failed"));
    assert_eq!(
        fixture
            .git_output(
                &fixture.main,
                &["config", "--worktree", "--get", "core.hooksPath"]
            )
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn repository_format_hazards_and_unowned_reserved_paths_fail_before_mutation() {
    let fixture = Fixture::new();
    fixture.git(
        &fixture.main,
        &["config", "extensions.seekdeepUnknown", "true"],
    );
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("dormant repository extension extensions.seekdeepunknown"));
    assert!(!fixture.hooks_path(&fixture.main).exists());

    let fixture = Fixture::new();
    let reserved = fixture.hooks_path(&fixture.main).join("pre-commit");
    fixture.write(&reserved, "#!/bin/sh\n# user content\n", Some(0o755));
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("refusing to overwrite unowned hooks directory"));
    assert_eq!(
        fs::read_to_string(reserved).unwrap(),
        "#!/bin/sh\n# user content\n"
    );
}

#[test]
fn copied_and_relocated_owned_hook_paths_are_repaired() {
    let fixture = Fixture::new();
    fixture.install(&fixture.main).unwrap();
    let main_hooks = fixture.hooks_path(&fixture.main);
    let late = fixture.container.path().join("late-linked");
    fixture.git_owned(
        &fixture.main,
        &[
            "worktree".into(),
            "add".into(),
            "--quiet".into(),
            "-b".into(),
            "late".into(),
            late.as_os_str().to_owned(),
        ],
    );
    fixture.write(&late.join("lefthook.yml"), "late-config\n", None);
    fixture.install_fake_lefthook(&late);
    assert_eq!(
        fixture.git(&late, &["config", "--worktree", "--get", "core.hooksPath"]),
        main_hooks.to_string_lossy()
    );
    fixture.install(&late).unwrap();
    assert_ne!(fixture.hooks_path(&late), main_hooks);

    let fixture = Fixture::new();
    fixture.install(&fixture.main).unwrap();
    let old_hooks = fixture.hooks_path(&fixture.main);
    let moved = fixture.container.path().join("moved-main");
    fs::rename(&fixture.main, &moved).unwrap();
    fixture.install_fake_lefthook(&moved);
    fixture.install(&moved).unwrap();
    let moved_hooks = fixture.hooks_path(&moved);
    assert_ne!(moved_hooks, old_hooks);
    assert_eq!(
        fixture.git(&moved, &["config", "--worktree", "--get", "core.hooksPath"]),
        moved_hooks.to_string_lossy()
    );
    let marker = fs::read_to_string(moved_hooks.join(".seekdeep-lefthook-owned")).unwrap();
    assert!(marker.contains(&serde_json::to_string(&moved_hooks.to_string_lossy()).unwrap()));
}

#[test]
fn command_scoped_git_config_is_removed_before_lefthook_runs() {
    let fixture = Fixture::new();
    let mut options = fixture.options(&fixture.main);
    options.environment.insert(
        "SEEKDEEP_TEST_FORBIDDEN_GIT_CONFIG_KEY".into(),
        "seekdeep.testSentinel".into(),
    );
    options
        .environment
        .insert("GIT_CONFIG_COUNT".into(), "1".into());
    options
        .environment
        .insert("GIT_CONFIG_KEY_0".into(), "seekdeep.testSentinel".into());
    options
        .environment
        .insert("GIT_CONFIG_VALUE_0".into(), "forbidden".into());
    install_lefthook(&fixture.main, &options).unwrap();
    assert!(
        fixture
            .hooks_path(&fixture.main)
            .join("pre-commit")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn symlinked_configs_and_aliased_owned_entries_are_refused() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let common = fixture.common_directory().join("config");
    let external = fixture.container.path().join("external-config");
    fs::rename(&common, &external).unwrap();
    symlink(&external, &common).unwrap();
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("common repository config"));
    assert!(error.contains("not a regular file"));

    let fixture = Fixture::new();
    fixture.install(&fixture.main).unwrap();
    let hook = fixture.hooks_path(&fixture.main).join("pre-commit");
    let outside = fixture.container.path().join("outside-hook");
    fs::write(&outside, "outside\n").unwrap();
    fs::remove_file(&hook).unwrap();
    symlink(&outside, &hook).unwrap();
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("non-regular or multiply linked hook entry"));
    assert_eq!(fs::read_to_string(outside).unwrap(), "outside\n");
}

#[test]
fn postinstall_binary_skips_automated_jobs_before_mutation() {
    let fixture = Fixture::new();
    let binary = env!("CARGO_BIN_EXE_install-lefthook");
    for key in ["CI", "GITHUB_ACTIONS"] {
        let mut command = Command::new(binary);
        command
            .current_dir(&fixture.main)
            .env_clear()
            .envs(&fixture.environment)
            .env(key, "true");
        let output = command.output().unwrap();
        assert_success(&output);
        assert!(!fixture.hooks_path(&fixture.main).exists());
    }
}

#[test]
fn partially_published_lock_is_waited_out_and_changed_ownership_is_not_removed() {
    let fixture = std::sync::Arc::new(Fixture::new());
    let mut publishing = fixture.options(&fixture.main);
    publishing.environment.insert(
        "SEEKDEEP_TEST_LEFTHOOK_LOCK_WRITE_DELAY_MS".into(),
        "200".into(),
    );
    let main = fixture.main.clone();
    let first = std::thread::spawn(move || install_lefthook(&main, &publishing));
    wait_for_path(&fixture.install_lock_path());
    assert_eq!(fs::read_to_string(fixture.install_lock_path()).unwrap(), "");
    fixture.install(&fixture.linked).unwrap();
    first.join().unwrap().unwrap();
    assert!(!fixture.install_lock_path().exists());

    let fixture = std::sync::Arc::new(Fixture::new());
    let mut delayed = fixture.options(&fixture.main);
    delayed
        .environment
        .insert("SEEKDEEP_TEST_LEFTHOOK_DELAY_SECONDS".into(), "0.3".into());
    let main = fixture.main.clone();
    let install = std::thread::spawn(move || install_lefthook(&main, &delayed));
    wait_for_path(
        &fixture
            .hooks_path(&fixture.main)
            .join(".fake-lefthook-running"),
    );
    fs::write(fixture.install_lock_path(), "replacement owner\n").unwrap();
    let error = install.join().unwrap().unwrap_err().to_string();
    assert!(error.contains("installer lock ownership changed"));
    assert_eq!(
        fs::read_to_string(fixture.install_lock_path()).unwrap(),
        "replacement owner\n"
    );
}

#[test]
fn dormant_and_included_worktree_configuration_is_never_activated_or_overwritten() {
    let fixture = Fixture::new();
    let linked_config = fixture
        .git_directory(&fixture.linked)
        .join("config.worktree");
    fixture.git(
        &fixture.main,
        &[
            "config",
            "--file",
            linked_config.to_str().unwrap(),
            "core.hooksPath",
            "linked-custom-hooks",
        ],
    );
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("sibling dormant worktree config"));
    assert!(!fixture.hooks_path(&fixture.main).exists());

    let fixture = Fixture::new();
    let common_config = fixture.common_directory().join("config");
    fixture.git(
        &fixture.main,
        &[
            "config",
            "--file",
            common_config.to_str().unwrap(),
            "core.repositoryFormatVersion",
            "1",
        ],
    );
    fixture.git(
        &fixture.main,
        &[
            "config",
            "--file",
            common_config.to_str().unwrap(),
            "extensions.worktreeConfig",
            "true",
        ],
    );
    let worktree_config = fixture.git_directory(&fixture.main).join("config.worktree");
    let included = fixture.container.path().join("included-worktree.gitconfig");
    let included_hooks = fixture.container.path().join("included-hooks");
    fixture.git(
        &fixture.main,
        &[
            "config",
            "--file",
            included.to_str().unwrap(),
            "core.hooksPath",
            included_hooks.to_str().unwrap(),
        ],
    );
    fixture.git(
        &fixture.main,
        &[
            "config",
            "--file",
            worktree_config.to_str().unwrap(),
            "include.path",
            included.to_str().unwrap(),
        ],
    );
    let error = fixture.install(&fixture.main).unwrap_err().to_string();
    assert!(error.contains("worktree-scoped core.hooksPath"));
    assert_eq!(
        fixture.git(&fixture.main, &["config", "--get", "core.hooksPath"]),
        included_hooks.to_string_lossy()
    );
}

#[test]
fn common_config_migration_ignores_values_loaded_only_through_includes() {
    let fixture = Fixture::new();
    let common_config = fixture.common_directory().join("config");
    let included = fixture.container.path().join("included-common.gitconfig");
    for (key, value) in [
        ("core.worktree", fixture.main.to_str().unwrap()),
        ("core.bare", "true"),
        ("extensions.seekdeepunknown", "true"),
    ] {
        fixture.git(
            &fixture.main,
            &["config", "--file", included.to_str().unwrap(), key, value],
        );
    }
    fixture.git(
        &fixture.main,
        &[
            "config",
            "--file",
            common_config.to_str().unwrap(),
            "include.path",
            included.to_str().unwrap(),
        ],
    );
    fixture.install(&fixture.linked).unwrap();
    assert!(
        fixture
            .hooks_path(&fixture.linked)
            .join("pre-commit")
            .exists()
    );
}

#[test]
fn marker_outside_a_registered_worktree_path_does_not_grant_ownership() {
    let fixture = Fixture::new();
    fixture.install(&fixture.main).unwrap();
    let external_hooks = fixture.container.path().join("external-owned-hooks");
    fixture.write(
        &external_hooks.join(".seekdeep-lefthook-owned"),
        &format!(
            "{}\n",
            serde_json::json!({
                "version": 1,
                "owner": "seekdeep-harness worktree-local lefthook hooks",
                "hooksPath": external_hooks,
            })
        ),
        Some(0o600),
    );
    fixture.git(
        &fixture.linked,
        &[
            "config",
            "--worktree",
            "core.hooksPath",
            external_hooks.to_str().unwrap(),
        ],
    );
    let error = fixture.install(&fixture.linked).unwrap_err().to_string();
    assert!(error.contains("worktree-scoped core.hooksPath"));
    assert!(!fixture.hooks_path(&fixture.linked).exists());
}

#[test]
fn postinstall_binary_discovers_and_installs_the_real_rust_entrypoint() {
    let fixture = Fixture::new();
    let output = Command::new(env!("CARGO_BIN_EXE_install-lefthook"))
        .current_dir(&fixture.main)
        .env_clear()
        .envs(&fixture.environment)
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        fixture
            .hooks_path(&fixture.main)
            .join("pre-commit")
            .exists()
    );
    assert_eq!(
        fixture.git(
            &fixture.main,
            &[
                "config",
                "--worktree",
                "--get",
                "merge.seekdeep-translation-pairing.driver"
            ]
        ),
        "scripts/merge-translation-pairing-driver.sh %O %A %B %P"
    );
}

#[cfg(unix)]
#[test]
fn unsupported_git_is_rejected_before_repository_mutation() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new();
    let real_git = fixture.command_success("which", &["git"], &fixture.main);
    let fake_bin = fixture.container.path().join("fake-bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let fake_git = fake_bin.join("git");
    fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'git version 2.25.0'; exit 0; fi\nexec \"{real_git}\" \"$@\"\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755)).unwrap();
    let mut options = fixture.options(&fixture.main);
    let inherited_path = options
        .environment
        .get(OsStr::new("PATH"))
        .cloned()
        .unwrap_or_default();
    options.environment.insert(
        "PATH".into(),
        format!(
            "{}:{}",
            fake_bin.display(),
            inherited_path.to_string_lossy()
        )
        .into(),
    );
    let error = install_lefthook(&fixture.main, &options)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Git 2.26 or newer is required"));
    assert!(!fixture.hooks_path(&fixture.main).exists());
}

fn wait_for_path(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
