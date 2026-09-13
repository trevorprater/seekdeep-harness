//! Package-owned filesystem event-data invariants.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};

use crate::types::{FsObservation, FsTarget};

const PACKAGE_NAME: &str = "seekdeep-fs";

/// Registers checks over the filesystem decision and observation event stream.
///
/// # Errors
///
/// Returns ordinary invariant registration or installer failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(
            std::iter::empty::<String>(),
            |context, failure| async move { install(&context, &failure) },
        ),
    )
}

fn install(context: &Context, failure: &InvariantFailure) -> anyhow::Result<()> {
    let failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            let event_name = args
                .get::<String>(1)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
            if event_name.as_str() != "fs/write-intent"
                && event_name.as_str() != "fs/edit-intent"
                && event_name.as_str() != "fs/observed"
            {
                return Ok(EventReply::Undefined);
            }
            let carried = args
                .get::<EventArgs>(2)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
            let target = carried
                .get::<FsTarget>(0)
                .ok_or_else(|| anyhow::anyhow!("filesystem event lacks its target"))?;
            validate_target(&target, &failure)?;
            if event_name.as_str() == "fs/observed" {
                let observation = carried
                    .get::<FsObservation>(1)
                    .ok_or_else(|| anyhow::anyhow!("fs/observed lacks its observation"))?;
                if let FsObservation::Present { version } = observation.as_ref()
                    && version.as_str().is_empty()
                {
                    return Err(failure
                        .fail("fs/observed present version must be non-empty")
                        .into());
                }
            }
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn validate_target(target: &FsTarget, failure: &InvariantFailure) -> anyhow::Result<()> {
    if target.target_key.as_str().is_empty() {
        return Err(failure
            .fail("filesystem event targetKey must be non-empty")
            .into());
    }
    if target.display_path.is_empty() {
        return Err(failure
            .fail("filesystem event displayPath must be non-empty")
            .into());
    }
    Ok(())
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use seekdeep_invariants::InvariantConfig;

    use super::*;
    use crate::types::{FsTargetKey, FsVersion};

    fn target(target_key: &str, display_path: &str) -> FsTarget {
        FsTarget {
            target_key: FsTargetKey::new(target_key),
            display_path: display_path.to_owned(),
        }
    }

    fn emit(
        context: &Context,
        name: &str,
        target: FsTarget,
        observation: Option<FsObservation>,
    ) -> anyhow::Result<()> {
        let mut values: Vec<seekdeep_cordis::EventValue> = vec![Arc::new(target)];
        if let Some(observation) = observation {
            values.push(Arc::new(observation));
        }
        context
            .events()
            .emit(context, name, &EventArgs::from_values(values))
    }

    #[tokio::test]
    async fn rejects_empty_target_identity_and_present_version() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
        let registration = register_invariant(&registry).expect("registration");
        registration.await_ready().await.expect("ready");

        let empty_key =
            emit(&context, "fs/observed", target("", "x"), None).expect_err("empty targetKey");
        assert!(format!("{empty_key:#}").contains("targetKey must be non-empty"));

        let empty_display =
            emit(&context, "fs/observed", target("k", ""), None).expect_err("empty displayPath");
        assert!(format!("{empty_display:#}").contains("displayPath must be non-empty"));

        let empty_version = emit(
            &context,
            "fs/observed",
            target("k", "x"),
            Some(FsObservation::Present {
                version: FsVersion::new(""),
            }),
        )
        .expect_err("empty version");
        assert!(format!("{empty_version:#}").contains("present version must be non-empty"));
    }

    #[tokio::test]
    async fn accepts_valid_fs_events() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
        let registration = register_invariant(&registry).expect("registration");
        registration.await_ready().await.expect("ready");

        emit(&context, "fs/write-intent", target("k", "x"), None).expect("write intent");
        emit(
            &context,
            "fs/observed",
            target("k", "x"),
            Some(FsObservation::Absent),
        )
        .expect("absent observation");
        emit(
            &context,
            "fs/observed",
            target("k", "x"),
            Some(FsObservation::Present {
                version: FsVersion::new("v1"),
            }),
        )
        .expect("present observation");
    }
}
