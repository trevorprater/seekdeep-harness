//! Constrained preset copy, read, and deletion parity.

use std::path::{Path, PathBuf};

use seekdeep_agent_presets::{
    AgentPreset, COMPOSITION_FILE, InvalidPresetIdError, METADATA_FILE, PresetExistsError,
    PresetMetadata, PresetNotWritableError, PresetRoot, PresetTrust, copy_composition,
    delete_composition, read_composition, read_preset_metadata, render_preset_metadata,
    writable_root,
};

fn root(path: &Path, trust: PresetTrust) -> PresetRoot {
    PresetRoot {
        path: path.to_string_lossy().into_owned(),
        trust,
    }
}

async fn source_preset(
    root: &Path,
    id: &str,
    trust: PresetTrust,
    metadata: PresetMetadata,
) -> AgentPreset {
    let directory = root.join(id);
    tokio::fs::create_dir_all(directory.join("assets"))
        .await
        .unwrap();
    tokio::fs::write(
        directory.join(COMPOSITION_FILE),
        "- name: plugin\n  config: {}\n",
    )
    .await
    .unwrap();
    tokio::fs::write(directory.join("assets").join("data.txt"), "payload")
        .await
        .unwrap();
    if let Some(rendered) = render_preset_metadata(&metadata) {
        tokio::fs::write(directory.join(METADATA_FILE), rendered)
            .await
            .unwrap();
    }
    AgentPreset {
        id: id.to_owned(),
        trust,
        path: directory.join(COMPOSITION_FILE),
        name: metadata.name,
        description: metadata.description,
        order: metadata.order,
        broken: None,
    }
}

#[tokio::test]
async fn copy_moves_the_complete_tree_and_rewrites_display_identity() {
    let system = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let source = source_preset(
        system.path(),
        "standard",
        PresetTrust::System,
        PresetMetadata {
            name: Some("Standard".to_owned()),
            description: Some("Full coding preset.".to_owned()),
            order: Some(1.0),
        },
    )
    .await;
    let destination = copy_composition(
        &[
            root(system.path(), PresetTrust::System),
            root(user.path(), PresetTrust::User),
        ],
        &source,
        "mine",
        Some("My preset"),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(destination.join(COMPOSITION_FILE))
            .await
            .unwrap(),
        "- name: plugin\n  config: {}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(destination.join("assets/data.txt"))
            .await
            .unwrap(),
        "payload"
    );
    assert_eq!(
        read_preset_metadata(&destination).await,
        PresetMetadata {
            name: Some("My preset".to_owned()),
            description: Some("Full coding preset.".to_owned()),
            order: None,
        }
    );
}

#[tokio::test]
async fn copy_without_authored_name_never_inherits_source_name_or_order() {
    let system = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let source = source_preset(
        system.path(),
        "source",
        PresetTrust::System,
        PresetMetadata {
            name: Some("Source".to_owned()),
            description: None,
            order: Some(4.0),
        },
    )
    .await;
    let destination = copy_composition(
        &[root(user.path(), PresetTrust::User)],
        &source,
        "copy",
        None,
    )
    .await
    .unwrap();
    assert!(!destination.join(METADATA_FILE).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn copy_dereferences_symlinks_and_tightens_owner_modes() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let system = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let source = source_preset(
        system.path(),
        "source",
        PresetTrust::System,
        PresetMetadata::default(),
    )
    .await;
    let directory = source.path.parent().unwrap();
    let executable = directory.join("run.sh");
    tokio::fs::write(&executable, "#!/bin/sh\n").await.unwrap();
    tokio::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
        .await
        .unwrap();
    symlink("assets/data.txt", directory.join("linked.txt")).unwrap();
    let destination = copy_composition(
        &[root(user.path(), PresetTrust::User)],
        &source,
        "copy",
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(destination.join("linked.txt"))
            .await
            .unwrap(),
        "payload"
    );
    assert!(
        !tokio::fs::symlink_metadata(destination.join("linked.txt"))
            .await
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        tokio::fs::metadata(&destination)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        tokio::fs::metadata(destination.join("assets/data.txt"))
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        tokio::fs::metadata(destination.join("run.sh"))
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[tokio::test]
async fn invalid_and_occupied_ids_fail_without_overwriting() {
    let system = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let source = source_preset(
        system.path(),
        "source",
        PresetTrust::System,
        PresetMetadata::default(),
    )
    .await;
    for invalid in ["../escape", "Upper", "a/b", ""] {
        let error = copy_composition(
            &[root(user.path(), PresetTrust::User)],
            &source,
            invalid,
            None,
        )
        .await
        .unwrap_err();
        assert!(error.downcast_ref::<InvalidPresetIdError>().is_some());
    }
    let occupied = user.path().join("occupied");
    tokio::fs::create_dir(&occupied).await.unwrap();
    tokio::fs::write(occupied.join("sentinel"), "keep")
        .await
        .unwrap();
    let error = copy_composition(
        &[root(user.path(), PresetTrust::User)],
        &source,
        "occupied",
        None,
    )
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<PresetExistsError>().is_some());
    assert_eq!(
        tokio::fs::read_to_string(occupied.join("sentinel"))
            .await
            .unwrap(),
        "keep"
    );
}

#[tokio::test]
async fn first_copy_creates_the_user_root_and_failed_copy_rolls_back_target() {
    let system = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let user = parent.path().join("not-created-yet");
    let source = source_preset(
        system.path(),
        "source",
        PresetTrust::System,
        PresetMetadata::default(),
    )
    .await;
    copy_composition(&[root(&user, PresetTrust::User)], &source, "first", None)
        .await
        .unwrap();
    assert!(user.join("first").is_dir());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(
            "does-not-exist",
            source.path.parent().unwrap().join("broken-link"),
        )
        .unwrap();
        assert!(
            copy_composition(&[root(&user, PresetTrust::User)], &source, "failed", None,)
                .await
                .is_err()
        );
        assert!(!user.join("failed").exists());
    }
}

#[tokio::test]
async fn reading_and_deletion_preserve_the_writable_root_boundary() {
    let system = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let source = source_preset(
        system.path(),
        "system",
        PresetTrust::System,
        PresetMetadata::default(),
    )
    .await;
    assert!(read_composition(&source).await.unwrap().contains("plugin"));
    let roots = [root(user.path(), PresetTrust::User)];
    let destination = copy_composition(&roots, &source, "mine", None)
        .await
        .unwrap();
    let authored = AgentPreset {
        id: "mine".to_owned(),
        trust: PresetTrust::User,
        path: destination.join(COMPOSITION_FILE),
        name: None,
        description: None,
        order: None,
        broken: None,
    };
    delete_composition(&roots, &authored).await.unwrap();
    assert!(!destination.exists());
    let shipped = delete_composition(&roots, &source).await.unwrap_err();
    assert_eq!(
        shipped
            .downcast_ref::<PresetNotWritableError>()
            .unwrap()
            .reason,
        "it ships with the deployment"
    );

    let outside_dir = tempfile::tempdir().unwrap();
    let outside = AgentPreset {
        id: "outside".to_owned(),
        trust: PresetTrust::User,
        path: outside_dir.path().join(COMPOSITION_FILE),
        name: None,
        description: None,
        order: None,
        broken: None,
    };
    let refused = delete_composition(&roots, &outside).await.unwrap_err();
    assert_eq!(
        refused
            .downcast_ref::<PresetNotWritableError>()
            .unwrap()
            .reason,
        "it does not live under the writable preset root"
    );
}

#[test]
fn authoring_uses_the_first_user_root_and_refuses_when_none_exists() {
    let first = PathBuf::from("/first");
    let second = PathBuf::from("/second");
    assert_eq!(
        writable_root(&[
            PresetRoot {
                path: "/system".to_owned(),
                trust: PresetTrust::System,
            },
            PresetRoot {
                path: first.to_string_lossy().into_owned(),
                trust: PresetTrust::User,
            },
            PresetRoot {
                path: second.to_string_lossy().into_owned(),
                trust: PresetTrust::User,
            },
        ])
        .unwrap(),
        first
    );
    let error = writable_root(&[]).unwrap_err();
    assert!(error.downcast_ref::<PresetNotWritableError>().is_some());
}
