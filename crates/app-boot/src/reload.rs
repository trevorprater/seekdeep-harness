//! Serialized fine-grained composition reconciliation with last-good rollback.

use seekdeep_cordis::Context;
use seekdeep_loader::{LoadedComposition, PluginCatalog};

struct ReloadState {
    source: String,
    composition: Option<LoadedComposition>,
}

/// Owns one current config generation and transactionally replaces it.
pub struct ReloadableComposition {
    context: Context,
    state: tokio::sync::Mutex<ReloadState>,
}

impl std::fmt::Debug for ReloadableComposition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReloadableComposition")
            .field("fiber_state", &self.context.fiber().state())
            .finish_non_exhaustive()
    }
}

impl ReloadableComposition {
    /// Mounts the initial generation after parse/import preflight.
    ///
    /// # Errors
    ///
    /// Returns preflight or startup failures without retaining partial state.
    pub async fn open(
        context: Context,
        catalog: PluginCatalog,
        source: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let source = source.into();
        let composition = catalog.load_yaml(&context, &source).await?;
        Ok(Self {
            context,
            state: tokio::sync::Mutex::new(ReloadState {
                source,
                composition: Some(composition),
            }),
        })
    }

    /// Reconciles the tree and retains the last-good source on any failure.
    ///
    /// Calls serialize through candidate activation and rollback. Unknown
    /// plugin names fail during preflight while every old entry still runs;
    /// unaffected entries retain their exact fibers.
    ///
    /// # Errors
    ///
    /// Returns preflight, teardown, candidate, or rollback failures.
    pub async fn replace(&self, candidate: impl Into<String>) -> anyhow::Result<()> {
        let candidate = candidate.into();
        let mut state = self.state.lock().await;
        let composition = state
            .composition
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("reloadable composition is disposed"))?;
        composition.update_yaml(&candidate).await?;
        state.source = candidate;
        Ok(())
    }

    /// Exact source of the currently committed generation.
    pub async fn source(&self) -> String {
        self.state.lock().await.source.clone()
    }

    /// Disposes the current generation and rejects later replacement.
    ///
    /// # Errors
    ///
    /// Returns composition cleanup failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        let composition = self.state.lock().await.composition.take();
        match composition {
            Some(composition) => Ok(composition.dispose().await?),
            None => Ok(()),
        }
    }
}
