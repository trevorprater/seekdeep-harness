//! Event-only filesystem observation policy; it registers no service.

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_fs::{FsError, FsErrorCode, FsObservation, FsTarget, FsVersion, FsWriteIntent};
use seekdeep_tools::ToolExecution;

pub mod invariant;
pub mod types;

pub use types::ObservedOwner;

/// Cordis plugin name used by loader diagnostics.
pub const NAME: &str = "fs-observation-policy";

/// Derives the observed-state owner from the opaque event actor.
///
/// The owner is the actor's live agent session, treated as an opaque identity;
/// a direct call with no agent has no owner and reads freely but cannot satisfy
/// the write/edit prior-observation policy.
#[must_use]
pub fn observed_owner(actor: Option<&ToolExecution>) -> Option<ObservedOwner> {
    actor
        .and_then(|exec| exec.agent.as_ref())
        .map(|agent| ObservedOwner::new(Arc::as_ptr(agent.session()) as usize))
}

/// Per-context observed-file state and the three fs/* decisions over it.
#[derive(Debug, Default)]
pub struct ObservedStateGate {
    observed: Mutex<HashMap<ObservedOwner, HashMap<String, FsObservation>>>,
}

impl ObservedStateGate {
    /// Creates an empty gate.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, owner: ObservedOwner, target_key: &str) -> Option<FsObservation> {
        self.observed.lock().get(&owner)?.get(target_key).cloned()
    }

    fn set(&self, owner: ObservedOwner, target_key: &str, observation: FsObservation) {
        self.observed
            .lock()
            .entry(owner)
            .or_default()
            .insert(target_key.to_owned(), observation);
    }

    /// Drops all recorded state.
    pub fn clear(&self) {
        self.observed.lock().clear();
    }

    /// Decides the write intent: unseen or absent folds to createIfAbsent, present to replaceIfVersion.
    #[must_use]
    pub fn write_intent(&self, target: &FsTarget, owner: Option<ObservedOwner>) -> FsWriteIntent {
        let prior = owner.and_then(|owner| self.get(owner, target.target_key.as_str()));
        match prior {
            Some(FsObservation::Present { version }) => FsWriteIntent::ReplaceIfVersion { version },
            _ => FsWriteIntent::CreateIfAbsent,
        }
    }

    /// Decides the edit version guard; rejects unseen and absent targets.
    ///
    /// # Errors
    ///
    /// Returns `FS_NOT_OBSERVED` for an unseen target and `FS_NOT_FOUND` for an observed absence.
    pub fn edit_intent(
        &self,
        target: &FsTarget,
        owner: Option<ObservedOwner>,
    ) -> Result<FsVersion, FsError> {
        let prior = owner.and_then(|owner| self.get(owner, target.target_key.as_str()));
        match (owner, prior) {
            (None, _) | (Some(_), None) => Err(FsError::new(
                format!("edit requires reading {:?} first", target.display_path),
                FsErrorCode::FsNotObserved,
            )),
            (Some(_), Some(FsObservation::Absent)) => Err(FsError::new(
                format!("cannot edit {:?}: not found", target.display_path),
                FsErrorCode::FsNotFound,
            )),
            (Some(_), Some(FsObservation::Present { version })) => Ok(version),
        }
    }

    /// Records an authoritative present or absent observation.
    pub fn observe(
        &self,
        target: &FsTarget,
        observation: &FsObservation,
        owner: Option<ObservedOwner>,
    ) {
        if let Some(owner) = owner {
            self.set(owner, target.target_key.as_str(), observation.clone());
        }
    }
}

fn actor_arg(args: &EventArgs, index: usize) -> Option<Arc<ToolExecution>> {
    args.get::<ToolExecution>(index)
}

/// Builds the plugin value.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, std::iter::empty::<String>(), |context, _| {
        Box::pin(async move { apply(&context) })
    })
}

/// Registers the three fs/* listeners.
///
/// # Errors
///
/// Returns listener or teardown registration failures.
pub fn apply(context: &Context) -> anyhow::Result<()> {
    let gate = Arc::new(ObservedStateGate::new());

    context.own(EffectHandle::synchronous(
        "fs-observation-policy observed-state teardown",
        {
            let gate = gate.clone();
            move || {
                gate.clear();
                Ok(())
            }
        },
    ))?;

    let write_gate = gate.clone();
    context.events().on_waterfall(
        context,
        "fs/write-intent",
        move |_, args, _next| {
            let gate = write_gate.clone();
            Box::pin(async move {
                let target = args
                    .get::<FsTarget>(0)
                    .ok_or_else(|| anyhow::anyhow!("fs/write-intent lacks its target"))?;
                let actor = actor_arg(&args, 1);
                let intent = gate.write_intent(&target, observed_owner(actor.as_deref()));
                Ok(EventReply::Value(Arc::new(intent)))
            })
        },
        EventOptions::default(),
    )?;

    let edit_gate = gate.clone();
    context.events().on_waterfall(
        context,
        "fs/edit-intent",
        move |_, args, _next| {
            let gate = edit_gate.clone();
            Box::pin(async move {
                let target = args
                    .get::<FsTarget>(0)
                    .ok_or_else(|| anyhow::anyhow!("fs/edit-intent lacks its target"))?;
                let actor = actor_arg(&args, 1);
                let version = gate.edit_intent(&target, observed_owner(actor.as_deref()))?;
                Ok(EventReply::Value(Arc::new(version)))
            })
        },
        EventOptions::default(),
    )?;

    let observe_gate = gate;
    context.events().on_sync(
        context,
        "fs/observed",
        move |_, args| {
            let target = args
                .get::<FsTarget>(0)
                .ok_or_else(|| anyhow::anyhow!("fs/observed lacks its target"))?;
            let observation = args
                .get::<FsObservation>(1)
                .ok_or_else(|| anyhow::anyhow!("fs/observed lacks its observation"))?;
            let actor = actor_arg(&args, 2);
            observe_gate.observe(
                &target,
                observation.as_ref(),
                observed_owner(actor.as_deref()),
            );
            Ok(EventReply::Undefined)
        },
        EventOptions::default(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use seekdeep_fs::FsTargetKey;

    use super::*;

    fn target(path: &str) -> FsTarget {
        FsTarget {
            target_key: FsTargetKey::new(path),
            display_path: path.to_owned(),
        }
    }

    fn present(version: &str) -> FsObservation {
        FsObservation::Present {
            version: FsVersion::new(version),
        }
    }

    fn owner(identity: usize) -> ObservedOwner {
        ObservedOwner::new(identity)
    }

    #[test]
    fn write_intent_folds_unseen_and_absent_to_create() {
        let gate = ObservedStateGate::new();
        let t = target("a.txt");
        assert_eq!(gate.write_intent(&t, None), FsWriteIntent::CreateIfAbsent);
        assert_eq!(
            gate.write_intent(&t, Some(owner(1))),
            FsWriteIntent::CreateIfAbsent
        );
        gate.observe(&t, &present("v7"), Some(owner(1)));
        assert_eq!(
            gate.write_intent(&t, Some(owner(1))),
            FsWriteIntent::ReplaceIfVersion {
                version: FsVersion::new("v7"),
            }
        );
        gate.observe(&t, &FsObservation::Absent, Some(owner(1)));
        assert_eq!(
            gate.write_intent(&t, Some(owner(1))),
            FsWriteIntent::CreateIfAbsent
        );
    }

    #[test]
    fn edit_intent_rejects_unseen_and_absent_and_returns_version() {
        let gate = ObservedStateGate::new();
        let t = target("a.txt");
        assert_eq!(
            gate.edit_intent(&t, None).expect_err("no owner").code,
            FsErrorCode::FsNotObserved
        );
        assert_eq!(
            gate.edit_intent(&t, Some(owner(1)))
                .expect_err("unread")
                .code,
            FsErrorCode::FsNotObserved
        );
        gate.observe(&t, &FsObservation::Absent, Some(owner(1)));
        assert_eq!(
            gate.edit_intent(&t, Some(owner(1)))
                .expect_err("absent")
                .code,
            FsErrorCode::FsNotFound
        );
        gate.observe(&t, &present("v3"), Some(owner(1)));
        assert_eq!(
            gate.edit_intent(&t, Some(owner(1))).expect("observed"),
            FsVersion::new("v3")
        );
    }

    #[test]
    fn owners_are_isolated_and_clear_drops_state() {
        let gate = ObservedStateGate::new();
        let t = target("a.txt");
        gate.observe(&t, &present("v0"), Some(owner(1)));
        assert_eq!(
            gate.write_intent(&t, Some(owner(2))),
            FsWriteIntent::CreateIfAbsent
        );
        assert_eq!(
            gate.write_intent(&t, Some(owner(1))),
            FsWriteIntent::ReplaceIfVersion {
                version: FsVersion::new("v0"),
            }
        );
        gate.clear();
        assert_eq!(
            gate.edit_intent(&t, Some(owner(1)))
                .expect_err("cleared")
                .code,
            FsErrorCode::FsNotObserved
        );
    }
}
