//! Source-oracle coverage for cross-product Skill invocation policy.

use seekdeep_repository_tools::skill_invocation_metadata::inspect_skill_invocation_metadata;

fn write_skill(root: &std::path::Path, name: &str, frontmatter: &str, policy: &str) {
    let directory = root.join(".agents/skills").join(name);
    std::fs::create_dir_all(directory.join("agents")).unwrap();
    std::fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Test skill\n{frontmatter}---\n\nTest.\n"),
    )
    .unwrap();
    std::fs::write(
        directory.join("agents/openai.yaml"),
        format!("interface:\n  display_name: \"Test\"\n{policy}"),
    )
    .unwrap();
}

#[test]
fn accepts_aligned_default_and_manual_only_policies() {
    let root = tempfile::tempdir().unwrap();
    write_skill(root.path(), "default-skill", "", "");
    write_skill(
        root.path(),
        "manual-skill",
        "disable-model-invocation: true\nuser-invocable: true\n",
        "policy:\n  allow_implicit_invocation: false\n",
    );
    let report = inspect_skill_invocation_metadata(root.path()).unwrap();
    assert_eq!(report.pair_count, 2);
    assert!(report.violations.is_empty());
}

#[test]
fn rejects_both_directions_of_manual_only_policy_mismatch() {
    let root = tempfile::tempdir().unwrap();
    write_skill(
        root.path(),
        "claude-only",
        "disable-model-invocation: true\n",
        "",
    );
    write_skill(
        root.path(),
        "codex-only",
        "",
        "policy:\n  allow_implicit_invocation: false\n",
    );
    assert_eq!(
        inspect_skill_invocation_metadata(root.path())
            .unwrap()
            .violations,
        [
            ".agents/skills/claude-only: Claude Code manual-only=true but Codex manual-only=false",
            ".agents/skills/codex-only: Claude Code manual-only=false but Codex manual-only=true",
        ]
    );
}
