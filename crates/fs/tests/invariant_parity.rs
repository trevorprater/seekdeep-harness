//! Behavioral mirror of packages/fs/fs/tests/invariant.spec.ts.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs, EventValue};
use seekdeep_fs::{FsObservation, FsTarget, FsTargetKey, FsVersion, invariant::register_invariant};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};

async fn setup() -> Context {
    let context = Context::new();
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
    let registration = register_invariant(&registry).expect("register");
    registration.await_ready().await.expect("ready");
    context
}

fn target(key: &str, display_path: &str) -> FsTarget {
    FsTarget {
        target_key: FsTargetKey::new(key),
        display_path: display_path.to_owned(),
    }
}

fn emit(
    context: &Context,
    name: &str,
    target: FsTarget,
    observation: Option<FsObservation>,
) -> anyhow::Result<()> {
    let mut values: Vec<EventValue> = vec![Arc::new(target)];
    if let Some(observation) = observation {
        values.push(Arc::new(observation));
    }
    context
        .events()
        .emit(context, name, &EventArgs::from_values(values))
}

#[tokio::test]
async fn accepts_decision_and_observation_events_with_usable_identities() {
    let context = setup().await;
    emit(
        &context,
        "fs/write-intent",
        target("file:1", "file.txt"),
        None,
    )
    .expect("write intent");
    emit(
        &context,
        "fs/edit-intent",
        target("file:1", "file.txt"),
        None,
    )
    .expect("edit intent");
    emit(
        &context,
        "fs/observed",
        target("file:1", "file.txt"),
        Some(FsObservation::Present {
            version: FsVersion::new("v1"),
        }),
    )
    .expect("present observation");
    emit(
        &context,
        "fs/observed",
        target("file:1", "file.txt"),
        Some(FsObservation::Absent),
    )
    .expect("absent observation");
    context
        .events()
        .emit(&context, "tools/change", &EventArgs::new())
        .expect("unrelated");
}

#[tokio::test]
async fn rejects_empty_target_and_version_identities() {
    let context = setup().await;

    let error = emit(
        &context,
        "fs/observed",
        target("", "file.txt"),
        Some(FsObservation::Present {
            version: FsVersion::new("v1"),
        }),
    )
    .expect_err("empty targetKey");
    assert!(error.to_string().contains("targetKey must be non-empty"));

    let error = emit(
        &context,
        "fs/observed",
        target("file:1", ""),
        Some(FsObservation::Present {
            version: FsVersion::new("v1"),
        }),
    )
    .expect_err("empty displayPath");
    assert!(error.to_string().contains("displayPath must be non-empty"));

    let error = emit(
        &context,
        "fs/observed",
        target("file:1", "file.txt"),
        Some(FsObservation::Present {
            version: FsVersion::new(""),
        }),
    )
    .expect_err("empty version");
    assert!(
        error
            .to_string()
            .contains("present version must be non-empty")
    );
}
