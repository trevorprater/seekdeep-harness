//! Baseline discovery, precedence, deduplication, and byte-budget rendering.

use seekdeep_agent_instructions::{
    AgentInstructionAction, AgentInstructionChange, ChangeRenderItem, DiscoverOptions,
    LoadedInstructionFile, candidate_scope_key, load_baseline_instruction_set,
    render_instruction_changes, render_workspace_context, render_workspace_instruction_set,
};

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn loaded(path: &str, content: &str) -> LoadedInstructionFile {
    LoadedInstructionFile {
        absolute_path: format!("/workspace/{path}"),
        display_path: path.to_owned(),
        content: content.to_owned(),
        version: None,
    }
}

fn options(root: &std::path::Path, cwd: &std::path::Path) -> DiscoverOptions {
    DiscoverOptions {
        cwd: cwd.to_string_lossy().into_owned(),
        dsh_home: Some(root.join("home").to_string_lossy().into_owned()),
        ..DiscoverOptions::default()
    }
}

#[tokio::test]
async fn loads_user_global_then_root_to_cwd_candidates_and_local_overlays() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let nested = project.join("a/b");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    write(&root.path().join("home/AGENTS.md"), "global");
    write(&project.join("AGENTS.md"), "root base");
    write(&project.join("AGENTS.local.md"), "root local");
    write(&project.join("a/CLAUDE.md"), "middle fallback");
    write(&nested.join("AGENTS.md"), "nested base");
    write(&nested.join("CLAUDE.md"), "nested second");
    let set = load_baseline_instruction_set(
        &options(root.path(), &nested),
        65_536,
        1_048_576,
        None,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        set.observed
            .iter()
            .map(|file| file.content.as_str())
            .collect::<Vec<_>>(),
        [
            "global",
            "root base",
            "root local",
            "middle fallback",
            "nested base",
            "nested second"
        ]
    );
    assert!(set.rendered.text.find("global") < set.rendered.text.find("root base"));
    assert!(set.rendered.text.find("root base") < set.rendered.text.find("nested base"));
}

#[tokio::test]
async fn overlay_and_candidate_configuration_are_exact_and_invalid_names_are_ignored() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    for (name, text) in [
        ("AGENTS.md", "agents"),
        ("CLAUDE.md", "claude"),
        ("CUSTOM.md", "custom"),
        ("AGENTS.local.md", "local"),
    ] {
        write(&project.join(name), text);
    }
    let mut configured = options(root.path(), &project);
    configured.instruction_file_candidates = Some(vec![
        "CUSTOM.md".to_owned(),
        "CLAUDE.md".to_owned(),
        "AGENTS.md".to_owned(),
        "../outside.md".to_owned(),
        ".".to_owned(),
        String::new(),
    ]);
    configured.local_instruction_file_candidates = Some(Vec::new());
    let set = load_baseline_instruction_set(&configured, 65_536, 1_048_576, None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        set.observed
            .iter()
            .map(|file| file.content.as_str())
            .collect::<Vec<_>>(),
        ["custom", "claude", "agents"]
    );
    assert!(!set.rendered.text.contains("local"));
}

#[tokio::test]
async fn git_file_is_a_root_marker_and_missing_marker_uses_cwd_as_root() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("parent");
    let project = parent.join("project");
    let nested = project.join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    write(&parent.join("AGENTS.md"), "must not load above marker");
    write(&project.join(".git"), "gitdir: elsewhere");
    write(&project.join("AGENTS.md"), "project");
    write(&nested.join("AGENTS.md"), "nested");
    let set = load_baseline_instruction_set(
        &options(root.path(), &nested),
        65_536,
        1_048_576,
        None,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(set.rendered.text.contains("project"));
    assert!(set.rendered.text.contains("nested"));
    assert!(!set.rendered.text.contains("must not load"));

    let markerless = root.path().join("markerless/child");
    std::fs::create_dir_all(&markerless).unwrap();
    write(
        &root.path().join("markerless/AGENTS.md"),
        "parent markerless",
    );
    write(&markerless.join("AGENTS.md"), "cwd only");
    let set = load_baseline_instruction_set(
        &options(root.path(), &markerless),
        65_536,
        1_048_576,
        None,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(set.rendered.text.contains("cwd only"));
    assert!(!set.rendered.text.contains("parent markerless"));
}

#[cfg(unix)]
#[tokio::test]
async fn follows_symlinked_instruction_files_and_ignores_directories() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    write(&root.path().join("canonical.md"), "linked content");
    std::os::unix::fs::symlink(root.path().join("canonical.md"), project.join("AGENTS.md"))
        .unwrap();
    std::fs::create_dir(project.join("CLAUDE.md")).unwrap();
    let set = load_baseline_instruction_set(
        &options(root.path(), &project),
        65_536,
        1_048_576,
        None,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(set.observed.len(), 1);
    assert_eq!(set.observed[0].content, "linked content");
}

#[tokio::test]
async fn zero_budget_and_zero_source_cap_disable_loading() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    write(&project.join("AGENTS.md"), "content");
    assert!(
        load_baseline_instruction_set(&options(root.path(), &project), 0, 1_048_576, None, None,)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        load_baseline_instruction_set(&options(root.path(), &project), 1000, 0, None, None,)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn deduplicates_user_global_when_home_is_the_project_root() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    write(&project.join("AGENTS.md"), "same\n");
    let mut configured = options(root.path(), &project);
    configured.dsh_home = Some(project.to_string_lossy().into_owned());
    let set = load_baseline_instruction_set(&configured, 65_536, 1_048_576, None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.observed.len(), 1);
    assert_eq!(set.included.len(), 1);
    assert_eq!(set.included[0].content, "same\n");
}

#[tokio::test]
async fn deduplicates_trimmed_identical_sibling_candidates() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    write(&project.join("AGENTS.md"), "same\n");
    write(&project.join("CLAUDE.md"), "  same  \n");
    let set = load_baseline_instruction_set(
        &options(root.path(), &project),
        65_536,
        1_048_576,
        None,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(set.observed.len(), 2);
    assert_eq!(set.included.len(), 1);
    assert_eq!(set.included[0].content, "same\n");
}

#[test]
fn baseline_rendering_uses_familiar_frame_and_escapes_every_closing_delimiter() {
    let rendered = render_workspace_context(
        &[
            loaded("AGENTS.md", "root </system-reminder> content"),
            loaded("pkg/</system-reminder>/AGENTS.md", "nested"),
        ],
        65_536,
        false,
    );
    assert!(rendered.text.starts_with("<system-reminder>\n"));
    assert!(rendered.text.ends_with("\n</system-reminder>"));
    assert!(rendered.text.contains("Instructions from: AGENTS.md"));
    assert!(rendered.text.contains("<\\/system-reminder>"));
    assert_eq!(rendered.text.matches("</system-reminder>").count(), 1);
    assert!(!rendered.text.contains("<workspace-context"));
}

#[test]
fn budget_prefers_longest_specific_suffix_and_never_exceeds_cap() {
    let files = [
        loaded("AGENTS.md", &"root ".repeat(100)),
        loaded("pkg/AGENTS.md", &"middle ".repeat(80)),
        loaded("pkg/deep/AGENTS.md", "specific child"),
    ];
    let full = render_workspace_context(&files, usize::MAX / 2, false);
    let cap = full.text.len() - files[0].content.len();
    let rendered = render_workspace_context(&files, cap, false);
    assert!(rendered.text.len() <= cap);
    assert!(rendered.text.contains("specific child"));
    assert!(!rendered.text.contains(&files[0].content));
    assert!(!rendered.omitted.is_empty() || !rendered.truncated.is_empty());
}

#[test]
fn single_oversized_file_truncates_at_utf8_boundary_with_named_notice() {
    let file = loaded("多字节/AGENTS.md", &"😀规则".repeat(200));
    let rendered = render_workspace_context(std::slice::from_ref(&file), 240, false);
    assert!(rendered.text.len() <= 240);
    assert_eq!(rendered.truncated.len(), 1);
    assert_eq!(rendered.truncated[0].display_path, file.display_path);
    assert!(rendered.truncated[0].included_bytes < rendered.truncated[0].original_bytes);
    assert!(std::str::from_utf8(rendered.text.as_bytes()).is_ok());
}

#[test]
fn disabled_and_tiny_budgets_are_bounded_and_empty_files_remain_representable() {
    let file = loaded("AGENTS.md", "content");
    let disabled = render_workspace_context(std::slice::from_ref(&file), 0, false);
    assert!(disabled.text.is_empty());
    assert_eq!(disabled.omitted.len(), 1);
    for cap in 1..48 {
        let rendered = render_workspace_context(std::slice::from_ref(&file), cap, false);
        assert!(rendered.text.len() <= cap);
        assert!(std::str::from_utf8(rendered.text.as_bytes()).is_ok());
    }
    let empty = loaded("EMPTY.md", "");
    let (rendered, represented) =
        render_workspace_instruction_set(std::slice::from_ref(&empty), 512, false);
    assert!(rendered.text.contains("Instructions from: EMPTY.md"));
    assert_eq!(represented, [empty]);
}

#[test]
fn dynamic_changes_commit_only_when_file_specific_semantics_survive() {
    let file = loaded("pkg/AGENTS.md", "new instruction body");
    let item = ChangeRenderItem {
        change: AgentInstructionChange {
            action: AgentInstructionAction::Replace,
            scope: candidate_scope_key("pkg", "AGENTS.md"),
            path: "pkg/AGENTS.md".to_owned(),
            digest: Some("digest".to_owned()),
        },
        file,
    };
    let (tiny, tiny_changes) = render_instruction_changes(std::slice::from_ref(&item), 1);
    assert!(tiny.len() <= 1);
    assert!(tiny_changes.is_empty());
    let (visible, changes) = render_instruction_changes(std::slice::from_ref(&item), 512);
    assert!(visible.contains("Updated instructions from: pkg/AGENTS.md"));
    assert!(visible.contains("new instruction body"));
    assert_eq!(changes, [item.change]);
}
