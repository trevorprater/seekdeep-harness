//! Filesystem Skill discovery, precedence, frontmatter, and watcher invalidation.

use std::{path::Path, time::Duration};

use seekdeep_cordis::Context;
use seekdeep_skill::{
    Config as SkillConfig, SkillDefinition, SkillInvocationPolicy, SkillLookupOptions,
    SkillProvider as _, SkillRegistry, SkillSource, SkillSummary, SkillViewOptions,
};
use seekdeep_skill_filesystem::{Config, FileSystemSkillProvider, install};

async fn write_skill(root: &Path, entry: &str, frontmatter: &str, body: &str, flat: bool) {
    let path = if flat {
        root.join(format!("{entry}.md"))
    } else {
        let directory = root.join(entry);
        tokio::fs::create_dir_all(&directory).await.unwrap();
        directory.join("SKILL.md")
    };
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(path, format!("---\n{frontmatter}\n---\n\n{body}\n"))
        .await
        .unwrap();
}

fn options(cwd: Option<&Path>) -> SkillViewOptions {
    SkillViewOptions {
        lookup: SkillLookupOptions {
            cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
            signal: None,
        },
        scope: None,
    }
}

#[tokio::test]
async fn custom_root_discovers_bundles_flat_files_frontmatter_and_symlinks() {
    let context = Context::new();
    let registry = SkillRegistry::install(&context, &SkillConfig::default()).unwrap();
    let root = tempfile::tempdir().unwrap();
    write_skill(
        root.path(),
        "bundle",
        "name: bundled-skill\ndescription: Bundle\nwhenToUse: Often\ndisable-model-invocation: yes\nuser-invocable: 1\nmetadata:\n  owner: test",
        "bundle body",
        false,
    )
    .await;
    write_skill(
        root.path(),
        "flat",
        "name: flat-skill\ndescription: Flat",
        "flat body",
        true,
    )
    .await;
    write_skill(
        root.path(),
        "invalid",
        "name: Invalid Name\ndescription: ignored",
        "ignored",
        true,
    )
    .await;
    let (provider, _effect) = install(
        &context,
        Config {
            include_default_roots: false,
            custom_skill_dirs: vec![root.path().to_path_buf()],
            watch: false,
            ..Config::default()
        },
    )
    .unwrap();
    let observation = provider.list(&SkillLookupOptions::default()).await.unwrap();
    assert!(observation.complete);
    assert_eq!(
        observation
            .candidates
            .iter()
            .map(|candidate| candidate.name.as_str())
            .collect::<Vec<_>>(),
        ["bundled-skill", "flat-skill"]
    );
    let bundled = observation
        .candidates
        .iter()
        .find(|candidate| candidate.name == "bundled-skill")
        .unwrap();
    assert!(!bundled.invocation.model_invocable);
    assert!(bundled.invocation.user_invocable);
    assert_eq!(bundled.metadata.as_ref().unwrap()["owner"], "test");
    let definition = provider
        .get(bundled, &SkillLookupOptions::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(definition.content, "bundle body");
    assert_eq!(definition.summary.when_to_use.as_deref(), Some("Often"));
    assert_eq!(registry.list(&options(None)).await.unwrap().len(), 2);
}

#[tokio::test]
async fn project_then_runtime_then_custom_precedence_is_exact() {
    let context = Context::new();
    let registry = SkillRegistry::install(&context, &SkillConfig::default()).unwrap();
    registry
        .register(
            &context,
            SkillDefinition {
                summary: SkillSummary {
                    name: "shared".to_owned(),
                    description: "runtime".to_owned(),
                    when_to_use: None,
                    invocation: SkillInvocationPolicy {
                        model_invocable: true,
                        user_invocable: true,
                    },
                    source: SkillSource("runtime".to_owned()),
                    provider: "runtime".to_owned(),
                    resource_base: None,
                },
                content: "runtime".to_owned(),
                path: None,
                metadata: None,
            },
        )
        .unwrap();
    let project = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(project.path().join(".git"))
        .await
        .unwrap();
    let nested = project.path().join("src/deep");
    tokio::fs::create_dir_all(&nested).await.unwrap();
    let project_skills = project.path().join(".seekdeep/skills");
    write_skill(
        &project_skills,
        "project",
        "name: shared\ndescription: project",
        "project",
        false,
    )
    .await;
    let custom = tempfile::tempdir().unwrap();
    write_skill(
        custom.path(),
        "custom",
        "name: shared\ndescription: custom",
        "custom",
        false,
    )
    .await;
    install(
        &context,
        Config {
            custom_skill_dirs: vec![custom.path().to_path_buf()],
            watch: false,
            seekdeep_home: Some(tempfile::tempdir().unwrap().path().to_path_buf()),
            agents_home: Some(tempfile::tempdir().unwrap().path().to_path_buf()),
            ..Config::default()
        },
    )
    .unwrap();
    let listed = registry.list(&options(Some(&nested))).await.unwrap();
    assert_eq!(listed[0].description, "project");

    tokio::fs::remove_dir_all(&project_skills).await.unwrap();
    registry.invalidate();
    let without_project = registry.list(&options(Some(&nested))).await.unwrap();
    assert_eq!(without_project[0].description, "runtime");
}

#[tokio::test]
async fn watcher_invalidates_cached_empty_catalog_and_disposal_contains_late_changes() {
    let context = Context::new();
    let registry = SkillRegistry::install(&context, &SkillConfig::default()).unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("later/skills");
    let (provider, _effect) = install(
        &context,
        Config {
            include_default_roots: false,
            custom_skill_dirs: vec![root.clone()],
            watch: true,
            watch_use_polling: true,
            watch_stability_threshold_ms: 25,
            watch_poll_interval_ms: 25,
            ..Config::default()
        },
    )
    .unwrap();
    assert!(registry.list(&options(None)).await.unwrap().is_empty());
    write_skill(
        &root,
        "watched",
        "name: watched-skill\ndescription: Watched",
        "body",
        false,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if registry
                .list(&options(None))
                .await
                .unwrap()
                .iter()
                .any(|skill| skill.name == "watched-skill")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    provider.dispose();
    provider.dispose();
    write_skill(
        &root,
        "late",
        "name: late-skill\ndescription: Late",
        "late",
        false,
    )
    .await;
}

#[test]
fn invalid_provider_and_watcher_configuration_fails_at_construction() {
    let context = Context::new();
    let registry = SkillRegistry::install(&context, &SkillConfig::default()).unwrap();
    assert!(
        FileSystemSkillProvider::new(
            &context,
            &registry,
            Config {
                provider_name: String::new(),
                ..Config::default()
            }
        )
        .is_err()
    );
    assert!(
        FileSystemSkillProvider::new(
            &context,
            &registry,
            Config {
                watch_max_projects: 0,
                ..Config::default()
            }
        )
        .is_err()
    );
}
