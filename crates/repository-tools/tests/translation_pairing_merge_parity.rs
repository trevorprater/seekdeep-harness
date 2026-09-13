//! Three-way pairing composition, stopped-merge recovery, CLI, and launcher parity.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use seekdeep_repository_tools::{
    translation_pairing_git::{git_blob_hash, store_git_blob},
    translation_pairing_merge::{
        merge_translation_pairing_records, resolve_translation_pairing_conflicts,
    },
    translation_pairing_record::{
        TranslationPairingRecord, render_translation_pairing_record, translation_pair_paths,
    },
};

const BASE_SOURCE: &str = "# Guide\n\nEnglish | [中文](guide.zh.md)\n\nAlpha base.\n\nBeta base.\n";
const BASE_ZH: &str = "# 指南\n\n[English](guide.md) | 中文\n\n甲基础。\n\n乙基础。\n";
const CURRENT_SOURCE: &str =
    "# Guide\n\nEnglish | [中文](guide.zh.md)\n\nAlpha current.\n\nBeta base.\n";
const CURRENT_ZH: &str = "# 指南\n\n[English](guide.md) | 中文\n\n甲当前。\n\n乙基础。\n";
const OTHER_SOURCE: &str =
    "# Guide\n\nEnglish | [中文](guide.zh.md)\n\nAlpha base.\n\nBeta other.\n";
const OTHER_ZH: &str = "# 指南\n\n[English](guide.md) | 中文\n\n甲基础。\n\n乙对侧。\n";
const MERGED_SOURCE: &str =
    "# Guide\n\nEnglish | [中文](guide.zh.md)\n\nAlpha current.\n\nBeta other.\n";
const MERGED_ZH: &str = "# 指南\n\n[English](guide.md) | 中文\n\n甲当前。\n\n乙对侧。\n";

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new(pairing_attributes: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let output = Command::new("git")
            .args(["init", "--quiet", "--initial-branch=master"])
            .arg(root.path())
            .env("GIT_DEFAULT_HASH", "sha1")
            .output()
            .unwrap();
        assert_success(&output);
        let fixture = Self { root };
        fixture.git(&["config", "user.email", "pairing@example.test"]);
        fixture.git(&["config", "user.name", "Pairing Test"]);
        fixture.git(&["config", "core.attributesFile", "/dev/null"]);
        if pairing_attributes {
            fixture.write(
                ".gitattributes",
                "*.i18n.yaml merge=seekdeep-translation-pairing\n",
            );
        }
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        let absolute = self.path().join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(absolute, content).unwrap();
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.path().join(path)).unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(args)
            .output()
            .unwrap();
        assert_success(&output);
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn git_output(&self, args: &[&str]) -> Output {
        Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(args)
            .output()
            .unwrap()
    }

    fn record(&self, source_path: &str, source: &str, zh: &str) -> String {
        let paths = translation_pair_paths(source_path).unwrap();
        self.write(&paths.source, source);
        self.write(&paths.zh, zh);
        let record = render_translation_pairing_record(
            &paths,
            &TranslationPairingRecord {
                source_hash: store_git_blob(self.path(), source.as_bytes()).unwrap(),
                zh_hash: store_git_blob(self.path(), zh.as_bytes()).unwrap(),
            },
        );
        self.write(&paths.metadata, &record);
        record
    }

    fn commit_pair(&self, source: &str, zh: &str, message: &str) -> String {
        let record = self.record("docs/guide.md", source, zh);
        self.git(&["add", "."]);
        self.git(&["commit", "--quiet", "-m", message]);
        record
    }

    fn create_diverged_pair(&self) -> ThreeRecords {
        let ancestor = self.commit_pair(BASE_SOURCE, BASE_ZH, "base");
        self.git(&["switch", "--quiet", "-c", "current"]);
        let current = self.commit_pair(CURRENT_SOURCE, CURRENT_ZH, "current");
        self.git(&["switch", "--quiet", "master"]);
        let other = self.commit_pair(OTHER_SOURCE, OTHER_ZH, "other");
        self.git(&["switch", "--quiet", "current"]);
        ThreeRecords {
            ancestor,
            current,
            other,
        }
    }

    fn start_stopped_pairing_merge(&self) {
        self.create_diverged_pair();
        let merge = self.git_output(&["merge", "--no-commit", "master"]);
        assert_eq!(merge.status.code(), Some(1));
        assert_eq!(
            self.git(&["diff", "--name-only", "--diff-filter=U"]),
            "docs/guide.i18n.yaml"
        );
    }

    fn assert_merged_pair(&self) {
        assert_eq!(self.read("docs/guide.md"), MERGED_SOURCE);
        assert_eq!(self.read("docs/guide.zh.md"), MERGED_ZH);
        let paths = translation_pair_paths("docs/guide.md").unwrap();
        assert_eq!(
            self.read("docs/guide.i18n.yaml"),
            render_translation_pairing_record(
                &paths,
                &TranslationPairingRecord {
                    source_hash: git_blob_hash(MERGED_SOURCE.as_bytes()),
                    zh_hash: git_blob_hash(MERGED_ZH.as_bytes()),
                }
            )
        );
    }
}

struct ThreeRecords {
    ancestor: String,
    current: String,
    other: String,
}

#[test]
fn rejects_metadata_paths_outside_the_repository() {
    let fixture = Fixture::new(false);
    let error = merge_translation_pairing_records(fixture.path(), "../guide.i18n.yaml", "", "", "")
        .unwrap_err()
        .to_string();
    assert!(error.contains("pairing record escapes the repository"));
}

#[test]
fn merges_owner_blobs_named_by_three_valid_records() {
    let fixture = Fixture::new(false);
    fixture.git(&["config", "merge.default", "text"]);
    let records = fixture.create_diverged_pair();

    let result = merge_translation_pairing_records(
        fixture.path(),
        "docs/guide.i18n.yaml",
        &records.ancestor,
        &records.current,
        &records.other,
    )
    .unwrap();

    assert_eq!(result.source_content, MERGED_SOURCE.as_bytes());
    assert_eq!(result.zh_content, MERGED_ZH.as_bytes());
    assert_eq!(result.source_hash, git_blob_hash(MERGED_SOURCE.as_bytes()));
    assert_eq!(result.zh_hash, git_blob_hash(MERGED_ZH.as_bytes()));
}

#[test]
fn generated_source_may_omit_the_english_switcher() {
    let fixture = Fixture::new(false);
    let base_source = "# Module graph\n\nAlpha base.\n\nBeta base.\n";
    let base_zh = "# 模块图\n\n[English](module-graph.md) | 中文\n\n甲基础。\n\n乙基础。\n";
    let current_source = base_source.replace("Alpha base.", "Alpha current.");
    let current_zh = base_zh.replace("甲基础。", "甲当前。");
    let other_source = base_source.replace("Beta base.", "Beta other.");
    let other_zh = base_zh.replace("乙基础。", "乙对侧。");
    let ancestor = fixture.record("docs/module-graph.md", base_source, base_zh);
    let current = fixture.record("docs/module-graph.md", &current_source, &current_zh);
    let other = fixture.record("docs/module-graph.md", &other_source, &other_zh);

    let result = merge_translation_pairing_records(
        fixture.path(),
        "docs/module-graph.i18n.yaml",
        &ancestor,
        &current,
        &other,
    )
    .unwrap();

    assert_eq!(
        result.source_content,
        current_source
            .replace("Beta base.", "Beta other.")
            .as_bytes()
    );
    assert_eq!(
        result.zh_content,
        current_zh.replace("乙基础。", "乙对侧。").as_bytes()
    );
}

#[test]
fn required_switchers_and_structural_equivalence_fail_closed() {
    let fixture = Fixture::new(false);
    let source_without_switcher = BASE_SOURCE.replace("English | [中文](guide.zh.md)\n\n", "");
    let record = fixture.record("docs/guide.md", &source_without_switcher, BASE_ZH);
    let error = merge_translation_pairing_records(
        fixture.path(),
        "docs/guide.i18n.yaml",
        &record,
        &record,
        &record,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("docs/guide.md clean merge lost its language-switcher link"));

    let zh_without_switcher = BASE_ZH.replace("[English](guide.md) | 中文\n\n", "");
    let record = fixture.record("docs/guide.md", BASE_SOURCE, &zh_without_switcher);
    let error = merge_translation_pairing_records(
        fixture.path(),
        "docs/guide.i18n.yaml",
        &record,
        &record,
        &record,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("docs/guide.zh.md clean merge lost its language-switcher link"));

    let ancestor = fixture.record("docs/guide.md", BASE_SOURCE, BASE_ZH);
    let current = fixture.record("docs/guide.md", CURRENT_SOURCE, CURRENT_ZH);
    let structurally_other = format!("{OTHER_SOURCE}\n## Extra\n");
    let other = fixture.record("docs/guide.md", &structurally_other, OTHER_ZH);
    let error = merge_translation_pairing_records(
        fixture.path(),
        "docs/guide.i18n.yaml",
        &ancestor,
        &current,
        &other,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("clean merges diverge structurally"));
}

#[test]
fn content_conflicts_remain_for_a_human() {
    let fixture = Fixture::new(false);
    let ancestor = fixture.record("docs/guide.md", BASE_SOURCE, BASE_ZH);
    let current = fixture.record(
        "docs/guide.md",
        &BASE_SOURCE.replace("Alpha base.", "Alpha current."),
        &BASE_ZH.replace("甲基础。", "甲当前。"),
    );
    let other = fixture.record(
        "docs/guide.md",
        &BASE_SOURCE.replace("Alpha base.", "Alpha other."),
        &BASE_ZH.replace("甲基础。", "甲对侧。"),
    );
    let error = merge_translation_pairing_records(
        fixture.path(),
        "docs/guide.i18n.yaml",
        &ancestor,
        &current,
        &other,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("docs/guide.md has content conflicts"));
}

#[test]
fn non_default_owner_merge_strategies_are_rejected() {
    let fixture = Fixture::new(false);
    fixture.write(".gitattributes", "docs/*.md merge=custom-owner\n");
    let records = fixture.create_diverged_pair();
    let error = merge_translation_pairing_records(
        fixture.path(),
        "docs/guide.i18n.yaml",
        &records.ancestor,
        &records.current,
        &records.other,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("docs/guide.md uses merge=custom-owner"));

    let fixture = Fixture::new(false);
    fixture.git(&["config", "merge.default", "custom-owner"]);
    let records = fixture.create_diverged_pair();
    let error = merge_translation_pairing_records(
        fixture.path(),
        "docs/guide.i18n.yaml",
        &records.ancestor,
        &records.current,
        &records.other,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("merge.default=custom-owner"));
}

#[test]
fn stopped_merge_resolver_stages_a_clean_generated_record() {
    let fixture = Fixture::new(false);
    fixture.start_stopped_pairing_merge();

    assert_eq!(
        resolve_translation_pairing_conflicts(fixture.path()).unwrap(),
        ["docs/guide.i18n.yaml"]
    );
    assert_eq!(fixture.git(&["diff", "--name-only", "--diff-filter=U"]), "");
    fixture.assert_merged_pair();
}

#[test]
fn resolver_refuses_unstaged_owner_bytes_and_edited_sidecars() {
    let fixture = Fixture::new(false);
    fixture.start_stopped_pairing_merge();
    fixture.write("docs/guide.md", format!("{MERGED_SOURCE}\nunstaged\n"));
    let error = resolve_translation_pairing_conflicts(fixture.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("docs/guide.md has unstaged content"));
    assert_eq!(
        fixture.git(&["diff", "--name-only", "--diff-filter=U"]),
        "docs/guide.i18n.yaml"
    );

    let fixture = Fixture::new(false);
    fixture.start_stopped_pairing_merge();
    fixture.write("docs/guide.i18n.yaml", "manually resolved\n");
    let error = resolve_translation_pairing_conflicts(fixture.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("docs/guide.i18n.yaml has edited conflict content"));
    assert_eq!(fixture.read("docs/guide.i18n.yaml"), "manually resolved\n");
}

#[test]
fn resolver_stages_safe_records_while_aggregating_other_failures() {
    let fixture = Fixture::new(false);
    commit_mixed_pairs(
        &fixture,
        PairContent::base(),
        PairContent::manual_base(),
        "base",
    );
    fixture.git(&["switch", "--quiet", "-c", "current"]);
    commit_mixed_pairs(
        &fixture,
        PairContent::current(),
        PairContent::manual_current(),
        "current",
    );
    fixture.git(&["switch", "--quiet", "master"]);
    commit_mixed_pairs(
        &fixture,
        PairContent::other(),
        PairContent::manual_other(),
        "other",
    );
    fixture.git(&["switch", "--quiet", "current"]);
    let merge = fixture.git_output(&["merge", "--no-commit", "master"]);
    assert_eq!(merge.status.code(), Some(1));

    let error = resolve_translation_pairing_conflicts(fixture.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("resolved and staged docs/guide.i18n.yaml"));
    assert!(error.contains("docs/manual.i18n.yaml: docs/manual.md has content conflicts"));
    assert_eq!(
        fixture
            .git(&["diff", "--name-only", "--diff-filter=U"])
            .lines()
            .collect::<Vec<_>>(),
        [
            "docs/manual.i18n.yaml",
            "docs/manual.md",
            "docs/manual.zh.md"
        ]
    );
    fixture.assert_merged_pair();
}

#[test]
fn command_probe_resolve_and_recovery_diagnostics_match_the_contract() {
    let binary = env!("CARGO_BIN_EXE_merge-translation-pairing");
    let probe = Command::new(binary).arg("--probe").output().unwrap();
    assert_success(&probe);

    let fixture = Fixture::new(false);
    let resolve = Command::new(binary)
        .arg("--resolve")
        .current_dir(fixture.path())
        .output()
        .unwrap();
    assert_success(&resolve);
    assert_eq!(
        String::from_utf8_lossy(&resolve.stdout),
        "merge-translation-pairing: no unresolved pairing records\n"
    );

    let invalid = Command::new(binary)
        .current_dir(fixture.path())
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(stderr.contains("merge-driver mode requires"));
    assert!(stderr.contains("pnpm run verify-translation-pairing --write <pair>"));
    assert!(stderr.contains("pnpm run resolve-translation-pairing-conflicts"));
}

#[cfg(unix)]
#[test]
fn executable_launcher_drives_a_clean_git_merge_through_the_rust_binary() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new(true);
    fixture.create_diverged_pair();
    let fake_bin = fixture.path().join("fake-bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let fake_cargo = fake_bin.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nwhile [ \"$1\" != \"--\" ]; do shift; done\nshift\nexec \"$MERGE_BIN\" \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let driver = driver_script();
    assert_ne!(
        fs::metadata(&driver).unwrap().permissions().mode() & 0o111,
        0
    );
    fixture.git(&[
        "config",
        "merge.seekdeep-translation-pairing.driver",
        &format!("{} %O %A %B %P", shell_quote(&driver)),
    ]);
    let path = format!("{}:/usr/bin:/bin", fake_bin.to_string_lossy());
    let merge = Command::new("git")
        .arg("-C")
        .arg(fixture.path())
        .args(["merge", "--no-edit", "master"])
        .env("PATH", path)
        .env("MERGE_BIN", env!("CARGO_BIN_EXE_merge-translation-pairing"))
        .output()
        .unwrap();
    assert_success(&merge);
    assert_eq!(fixture.git(&["diff", "--name-only", "--diff-filter=U"]), "");
    fixture.assert_merged_pair();
}

#[cfg(unix)]
#[test]
fn rejecting_pre_merge_commit_hook_leaves_the_complete_result_staged() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new(true);
    fixture.create_diverged_pair();
    let fake_bin = fixture.path().join("fake-bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let fake_cargo = fake_bin.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nwhile [ \"$1\" != \"--\" ]; do shift; done\nshift\nexec \"$MERGE_BIN\" \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let driver = driver_script();
    fixture.git(&[
        "config",
        "merge.seekdeep-translation-pairing.driver",
        &format!("{} %O %A %B %P", shell_quote(&driver)),
    ]);
    let hooks = fixture.path().join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-merge-commit");
    fs::write(
        &hook,
        "#!/bin/sh\necho 'fixture pre-merge-commit rejection' >&2\nexit 77\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    fixture.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
    let head_before = fixture.git(&["rev-parse", "HEAD"]);
    let merge = Command::new("git")
        .arg("-C")
        .arg(fixture.path())
        .args(["merge", "--no-edit", "master"])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", fake_bin.to_string_lossy()),
        )
        .env("MERGE_BIN", env!("CARGO_BIN_EXE_merge-translation-pairing"))
        .output()
        .unwrap();
    assert_eq!(merge.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&merge.stderr).contains("fixture pre-merge-commit rejection"));
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), head_before);
    assert!(
        !fixture
            .git(&["rev-parse", "--verify", "MERGE_HEAD"])
            .is_empty()
    );
    assert_eq!(fixture.git(&["diff", "--name-only", "--diff-filter=U"]), "");
    assert!(
        fixture
            .git(&["diff", "--cached", "--name-only"])
            .lines()
            .any(|path| path == "docs/guide.i18n.yaml")
    );
    fixture.assert_merged_pair();
}

#[cfg(unix)]
#[test]
fn unavailable_rust_runtime_leaves_an_ordinary_recoverable_conflict() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new(true);
    fixture.create_diverged_pair();
    let fake_bin = fixture.path().join("fake-bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let fake_cargo = fake_bin.join("cargo");
    fs::write(&fake_cargo, "#!/bin/sh\nexit 72\n").unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let driver = driver_script();
    fixture.git(&[
        "config",
        "merge.seekdeep-translation-pairing.driver",
        &format!("{} %O %A %B %P", shell_quote(&driver)),
    ]);
    let merge = Command::new("git")
        .arg("-C")
        .arg(fixture.path())
        .args(["merge", "--no-commit", "master"])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", fake_bin.to_string_lossy()),
        )
        .output()
        .unwrap();
    assert_eq!(merge.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&merge.stderr)
            .contains("runtime is unavailable; leaving an ordinary text conflict")
    );
    assert_eq!(
        fixture.git(&["diff", "--name-only", "--diff-filter=U"]),
        "docs/guide.i18n.yaml"
    );
    assert!(fixture.read("docs/guide.i18n.yaml").contains("<<<<<<<"));
}

#[derive(Clone, Copy)]
struct PairContent<'a> {
    source: &'a str,
    zh: &'a str,
}

impl PairContent<'static> {
    fn base() -> Self {
        Self {
            source: BASE_SOURCE,
            zh: BASE_ZH,
        }
    }

    fn current() -> Self {
        Self {
            source: CURRENT_SOURCE,
            zh: CURRENT_ZH,
        }
    }

    fn other() -> Self {
        Self {
            source: OTHER_SOURCE,
            zh: OTHER_ZH,
        }
    }

    fn manual_base() -> Self {
        Self {
            source: "# Manual\n\nEnglish | [中文](manual.zh.md)\n\nAlpha base.\n",
            zh: "# 手册\n\n[English](manual.md) | 中文\n\n甲基础。\n",
        }
    }

    fn manual_current() -> Self {
        Self {
            source: "# Manual\n\nEnglish | [中文](manual.zh.md)\n\nAlpha current.\n",
            zh: "# 手册\n\n[English](manual.md) | 中文\n\n甲当前。\n",
        }
    }

    fn manual_other() -> Self {
        Self {
            source: "# Manual\n\nEnglish | [中文](manual.zh.md)\n\nAlpha other.\n",
            zh: "# 手册\n\n[English](manual.md) | 中文\n\n甲对侧。\n",
        }
    }
}

fn commit_mixed_pairs(
    fixture: &Fixture,
    guide: PairContent<'_>,
    manual: PairContent<'_>,
    message: &str,
) {
    fixture.record("docs/guide.md", guide.source, guide.zh);
    fixture.record("docs/manual.md", manual.source, manual.zh);
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "--quiet", "-m", message]);
}

fn driver_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/merge-translation-pairing-driver.sh")
}

fn shell_quote(path: &Path) -> String {
    format!("\"{}\"", path.to_string_lossy().replace('"', "\\\""))
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
