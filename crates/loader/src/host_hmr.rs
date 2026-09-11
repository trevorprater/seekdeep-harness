//! Host watcher classification and one replacement transaction per changed batch.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    CompositionRuntime, Entry, HostHmrOutcome, HostHmrReload, LoaderError, LoaderSettlement,
    module_paths_in_order,
};

/// Source Host HMR classification after config paths have been handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostHmrChange {
    /// Part of the launcher's dependency tree; requires a process restart.
    External,
    /// Present in a loaded module dependency graph; joins the debounce batch.
    Loaded,
    /// No loaded module observes the file; emits `hmr/change` directly.
    Untracked,
}

pub(super) fn include_paths(entries: &[Entry]) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    for entry in entries {
        if let Some(path) = entry
            .include
            .as_ref()
            .and_then(|include| include.resolved_path.as_ref())
        {
            paths.insert(path.clone());
        }
        paths.extend(include_paths(&entry.children));
    }
    paths
}

impl LoaderSettlement {
    /// Whether an exact file currently belongs to a configured include.
    #[must_use]
    pub fn is_include_path(&self, path: &Path) -> bool {
        let Ok(runtime) = self.attached() else {
            return false;
        };
        let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        runtime.include_paths.read().contains(&path)
            || include_paths(&crate::entry_specs(&runtime.programmatic.lock())).contains(&path)
    }

    /// Classifies a file without starting a replacement or emitting events.
    ///
    /// # Errors
    ///
    /// Returns an unavailable error after the Loader is disposed.
    pub fn classify_hmr_path(&self, path: &Path) -> Result<HostHmrChange, LoaderError> {
        let runtime = self.attached()?;
        let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        runtime.classify_hmr_path(&path)
    }

    /// Applies the source's single transaction for a debounced set of changes.
    ///
    /// # Errors
    ///
    /// Returns import, application, disposal, or best-effort rollback failures.
    pub async fn reload_modules(&self, paths: &[PathBuf]) -> Result<HostHmrOutcome, LoaderError> {
        self.attached()?.reload_modules(paths).await
    }
}

impl CompositionRuntime {
    fn classify_hmr_path(&self, path: &Path) -> Result<HostHmrChange, LoaderError> {
        if self.catalog.hmr_externals.read().contains(path) {
            return Ok(HostHmrChange::External);
        }
        let realm = self.catalog.node_realm.lock().clone();
        if let Some(realm) = realm
            && realm.contains(path)?
        {
            return Ok(HostHmrChange::Loaded);
        }
        Ok(
            if self
                .catalog
                .compatibility_dependencies
                .read()
                .values()
                .any(|paths| paths.contains(path))
            {
                HostHmrChange::Loaded
            } else {
                HostHmrChange::Untracked
            },
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "cache, registry, rollback, and publication follow one source transaction"
    )]
    pub(super) async fn reload_modules(
        &self,
        paths: &[PathBuf],
    ) -> Result<HostHmrOutcome, LoaderError> {
        let _hmr = self.catalog.hmr_transaction.lock().await;
        let changed: Vec<_> = paths
            .iter()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()))
            .collect();
        let classifications = changed
            .iter()
            .map(|path| self.classify_hmr_path(path))
            .collect::<Result<Vec<_>, _>>()?;
        if classifications.contains(&HostHmrChange::External) {
            return Ok(HostHmrOutcome::FullRestart);
        }
        let roots = {
            let entries = self.entries.lock();
            let entries = entries.as_ref().ok_or(LoaderError::Unavailable)?;
            let mut seen = BTreeSet::new();
            module_paths_in_order(entries)
                .into_iter()
                .chain(module_paths_in_order(&self.programmatic.lock()))
                .map(|path| path.to_string_lossy().into_owned())
                .filter(|key| seen.insert(key.clone()))
                .collect::<Vec<_>>()
        };
        if roots.is_empty() || !classifications.contains(&HostHmrChange::Loaded) {
            for path in changed {
                let _ = self.context.events().emit(
                    &self.context,
                    "hmr/change",
                    &seekdeep_cordis::EventArgs::one(path),
                );
            }
            return Ok(HostHmrOutcome::Untracked);
        }
        let mut fibers = self
            .entries
            .lock()
            .as_ref()
            .map(|entries| fiber_snapshots(entries))
            .ok_or(LoaderError::Unavailable)?;
        fibers.extend(fiber_snapshots(&self.programmatic.lock()));
        fibers.sort_by_key(|value| value["uid"].as_u64().unwrap_or(u64::MAX));
        let candidates = self.catalog.prepare_hmr_candidates(
            &changed,
            &roots,
            &serde_json::Value::Array(fibers),
        )?;
        let affected = candidates
            .iter()
            .map(|candidate| candidate.key.clone())
            .collect::<Vec<_>>();
        if affected.is_empty() {
            self.catalog.node_realm()?.finish(false)?;
            let _ = self.context.events().emit(
                &self.context,
                "hmr/reload",
                &seekdeep_cordis::EventArgs::one(HostHmrReload {
                    changed: changed.first().cloned().expect("a loaded change exists"),
                    entries: Vec::new(),
                }),
            );
            return Ok(HostHmrOutcome::Reloaded(Vec::new()));
        }
        let completion = self.settlement.begin();
        let (_operation, mut entries) = match self.take_entries().await {
            Ok(entries) => entries,
            Err(error) => {
                let _ = self.catalog.node_realm()?.finish(true);
                completion.finish(Err(Arc::from(error.to_string())));
                return Err(error);
            }
        };
        let reloads = candidates
            .iter()
            .map(|candidate| RuntimeReload {
                key: candidate.key.clone(),
                previous: candidate.previous_plugin.clone(),
                next: candidate.plugin.clone(),
                mounts: reload_mounts(self, &entries, &candidate.previous_plugin),
            })
            .collect::<Vec<_>>();
        let deferred = self.context.registry().defer_lifecycle();
        self.catalog.install_hmr_candidates(&candidates, false);
        let mut reloaded = Vec::new();
        let result = (|| {
            for reload in &reloads {
                if reload.mounts.is_empty() {
                    continue;
                }
                if let Some(previous) = self.context.registry().delete_with_disposal(
                    &reload.previous,
                    seekdeep_cordis::DisposalScheduling::Concurrent,
                ) {
                    self.hmr_retired.lock().extend(previous.fibers);
                }
                for mount in &reload.mounts {
                    rebind(self, &mut entries, mount, &reload.next)?;
                    reloaded.push(mount.id.clone());
                }
                self.context.logger(None).info([
                    serde_json::json!("reload plugin at %C"),
                    serde_json::json!(display_path(&self.context, &reload.key)),
                ]);
            }
            Ok::<(), LoaderError>(())
        })();
        match result {
            Ok(()) => {
                let finished = self.catalog.node_realm()?.finish(false);
                self.restore_entries(entries);
                if let Err(error) = finished {
                    completion.finish(Err(Arc::from(error.to_string())));
                    return Err(error);
                }
                let _ = self.context.events().emit(
                    &self.context,
                    "hmr/reload",
                    &seekdeep_cordis::EventArgs::one(HostHmrReload {
                        changed: changed.first().cloned().expect("a loaded change exists"),
                        entries: reloaded.clone(),
                    }),
                );
                drop(deferred);
                self.finish_hmr_settlement(completion)?;
                Ok(HostHmrOutcome::Reloaded(reloaded))
            }
            Err(error) => {
                let cache_rollback = self.catalog.node_realm()?.finish(true);
                self.catalog.install_hmr_candidates(&candidates, true);
                let mut failures = Vec::new();
                if let Err(cache) = cache_rollback {
                    failures.push(cache.to_string());
                }
                for reload in &reloads {
                    if reload.mounts.is_empty() {
                        continue;
                    }
                    self.context.registry().delete_with_disposal(
                        &reload.next,
                        seekdeep_cordis::DisposalScheduling::Concurrent,
                    );
                    for mount in &reload.mounts {
                        if let Err(rollback) = rebind(self, &mut entries, mount, &reload.previous) {
                            failures.push(rollback.to_string());
                        }
                    }
                }
                self.restore_entries(entries);
                drop(deferred);
                self.finish_hmr_settlement(completion)?;
                if failures.is_empty() {
                    Err(error)
                } else {
                    Err(LoaderError::Disposal(format!(
                        "{error}; Host HMR rollback failed: {}",
                        failures.join("; ")
                    )))
                }
            }
        }
    }

    fn finish_hmr_settlement(
        &self,
        completion: crate::LoaderSettlementCompletion,
    ) -> Result<(), LoaderError> {
        let runtime = self.settlement.attached()?;
        let fibers = runtime.fibers();
        tokio::spawn(async move {
            seekdeep_cordis::PluginFiber::await_all_quiescent(&fibers).await;
            let mut failures = Vec::new();
            for fiber in &fibers {
                if let Err(error) = fiber.await_settled().await {
                    failures.push(format!(
                        "failed to apply loader entry {} ({}): {error}",
                        fiber.entry_id().unwrap_or_default(),
                        fiber.entry_name().unwrap_or_default()
                    ));
                }
            }
            if let Some(entries) = runtime.entries.lock().as_ref() {
                *runtime.entry_snapshot.write() = crate::collect_entry_snapshot(entries);
            }
            runtime
                .hmr_retired
                .lock()
                .retain(|fiber| fiber.fiber().state() != seekdeep_cordis::FiberState::Disposed);
            completion.finish(if failures.is_empty() {
                Ok(())
            } else {
                Err(Arc::from(failures.join("; ")))
            });
        });
        Ok(())
    }
}

struct RuntimeReload {
    key: String,
    previous: seekdeep_cordis::Plugin,
    next: seekdeep_cordis::Plugin,
    mounts: Vec<ReloadMount>,
}

struct ReloadMount {
    id: crate::EntryId,
    owner: seekdeep_cordis::Context,
    config: serde_json::Value,
    inject: Vec<String>,
    module_path: Option<PathBuf>,
    programmatic: bool,
    order: Option<u64>,
}

fn reload_mounts(
    runtime: &CompositionRuntime,
    entries: &[crate::MountedEntry],
    plugin: &seekdeep_cordis::Plugin,
) -> Vec<ReloadMount> {
    let mut mounts = collect_mounts(entries, plugin, false);
    mounts.extend(collect_mounts(&runtime.programmatic.lock(), plugin, true));
    mounts.sort_by_key(|mount| mount.order);
    mounts
}

fn collect_mounts(
    entries: &[crate::MountedEntry],
    plugin: &seekdeep_cordis::Plugin,
    programmatic: bool,
) -> Vec<ReloadMount> {
    let mut mounts = Vec::new();
    for entry in entries {
        if let Some(fiber) = &entry.fiber
            && fiber.plugin_id() == plugin.id()
        {
            mounts.push(ReloadMount {
                id: entry.options.id.clone(),
                owner: entry.entry_context.clone(),
                config: fiber.config(),
                inject: entry.options.inject.clone(),
                module_path: entry.module_path.clone(),
                programmatic,
                order: fiber.uid(),
            });
        }
        mounts.extend(collect_mounts(&entry.children, plugin, programmatic));
    }
    mounts
}

fn mounted_entry<'a>(
    entries: &'a mut [crate::MountedEntry],
    mount: &ReloadMount,
) -> Option<&'a mut crate::MountedEntry> {
    for entry in entries {
        if entry.options.id == mount.id
            && Arc::ptr_eq(entry.entry_context.fiber(), mount.owner.fiber())
        {
            return Some(entry);
        }
        if let Some(entry) = mounted_entry(&mut entry.children, mount) {
            return Some(entry);
        }
    }
    None
}

fn rebind(
    runtime: &CompositionRuntime,
    entries: &mut [crate::MountedEntry],
    mount: &ReloadMount,
    plugin: &seekdeep_cordis::Plugin,
) -> Result<(), LoaderError> {
    let environment = runtime.catalog.expressions.clone();
    let plugin = plugin
        .clone()
        .with_additional_inject(mount.inject.clone())
        .with_config_resolver(move |context, raw| {
            crate::expression::interpolate_config(&environment, context, raw)
        });
    let result = mount.owner.plugin(plugin.clone(), mount.config.clone());
    let outcome = if mount.programmatic {
        record_replacement(&mut runtime.programmatic.lock(), mount, plugin, result)
    } else {
        record_replacement(entries, mount, plugin, result)
    };
    if outcome.is_err() {
        mount.owner.logger(None).warn([
            serde_json::json!("failed to reload plugin at %C"),
            serde_json::json!(mount.module_path.as_ref().map_or_else(String::new, |path| {
                display_path(&mount.owner, &path.to_string_lossy())
            })),
        ]);
    }
    outcome
}

fn record_replacement(
    entries: &mut [crate::MountedEntry],
    mount: &ReloadMount,
    plugin: seekdeep_cordis::Plugin,
    result: Result<Arc<seekdeep_cordis::PluginFiber>, seekdeep_cordis::CordisError>,
) -> Result<(), LoaderError> {
    let entry = mounted_entry(entries, mount).ok_or(LoaderError::Unavailable)?;
    match result {
        Ok(fiber) => {
            entry.fiber = Some(fiber);
            entry.plugin = Some(plugin);
            Ok(())
        }
        Err(error) => {
            entry.options.disabled = true;
            entry.effective_disabled = true;
            let message = match error {
                seekdeep_cordis::CordisError::PluginPublication(message) => message,
                error => error.to_string(),
            };
            Err(LoaderError::StructuredModuleLoad {
                message: message.clone(),
                error: serde_json::json!({"name":"Error","message":message}),
            })
        }
    }
}

fn display_path(context: &seekdeep_cordis::Context, path: &str) -> String {
    let base = context
        .meta("loader.base_url")
        .and_then(|value| value.as_str().and_then(|value| url::Url::parse(value).ok()))
        .and_then(|url| url.to_file_path().ok());
    base.and_then(|base| {
        Path::new(path)
            .strip_prefix(base)
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    })
    .unwrap_or_else(|| path.to_owned())
}

fn fiber_snapshots(entries: &[crate::MountedEntry]) -> Vec<serde_json::Value> {
    let mut snapshots = Vec::new();
    for entry in entries {
        if let (Some(path), Some(fiber)) = (&entry.module_path, &entry.fiber) {
            let state = match fiber.fiber().state() {
                seekdeep_cordis::FiberState::Pending => 0,
                seekdeep_cordis::FiberState::Loading => 1,
                seekdeep_cordis::FiberState::Active => 2,
                seekdeep_cordis::FiberState::Failed => 3,
                seekdeep_cordis::FiberState::Disposed => 4,
                seekdeep_cordis::FiberState::Unloading => 5,
            };
            snapshots.push(serde_json::json!({"path":path,"key":fiber.fiber().id().to_string(),"uid":fiber.uid(),"state":state,"config":fiber.config(),"entry":{"id":entry.options.id,"options":{"name":entry.options.plugin,"config":entry.options.config,"disabled":entry.options.disabled}}}));
        }
        snapshots.extend(fiber_snapshots(&entry.children));
    }
    snapshots.sort_by_key(|value| value["uid"].as_u64().unwrap_or(u64::MAX));
    snapshots
}
