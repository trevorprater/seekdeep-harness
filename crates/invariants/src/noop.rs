//! Shared implementation for intentionally empty package invariant companions.

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin, PluginFiber, fiber::EffectHandle};

use crate::{INVARIANTS, InvariantInstaller, InvariantRegistration, InvariantRegistry};

mod catalog;

pub use catalog::NOOP_INVARIANTS;

/// Service required by every package invariant companion.
pub const INJECT: &[&str] = &["invariants"];

/// Exact metadata and lifecycle behavior for one source no-op companion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NoopInvariantDescriptor {
    source_surface: &'static str,
    plugin_name: &'static str,
    package_name: &'static str,
}

impl NoopInvariantDescriptor {
    /// Declares one source-derived no-op invariant companion.
    #[must_use]
    pub const fn new(
        source_surface: &'static str,
        plugin_name: &'static str,
        package_name: &'static str,
    ) -> Self {
        Self {
            source_surface,
            plugin_name,
            package_name,
        }
    }

    /// Pinned source surface represented by this descriptor.
    #[must_use]
    pub const fn source_surface(self) -> &'static str {
        self.source_surface
    }

    /// Product-renamed Cordis plugin name.
    #[must_use]
    pub const fn plugin_name(self) -> &'static str {
        self.plugin_name
    }

    /// Product-renamed package identity reserved by the registry.
    #[must_use]
    pub const fn package_name(self) -> &'static str {
        self.package_name
    }

    /// Registers this package's explained empty installer directly.
    ///
    /// # Errors
    ///
    /// Returns ordinary invariant registry failures.
    pub fn register(
        self,
        registry: &Arc<InvariantRegistry>,
    ) -> anyhow::Result<InvariantRegistration> {
        registry.register(self.package_name, InvariantInstaller::noop())
    }

    /// Builds the loader-compatible companion plugin.
    #[must_use]
    pub fn plugin(self) -> Plugin {
        Plugin::new(
            self.plugin_name,
            INJECT.iter().copied(),
            move |context, _| Box::pin(async move { self.apply(&context).await }),
        )
    }

    /// Mounts the companion as a lifecycle-owned plugin fiber.
    ///
    /// # Errors
    ///
    /// Returns inactive-context failures.
    pub fn install(self, context: &Context) -> anyhow::Result<Arc<PluginFiber>> {
        Ok(context.plugin(self.plugin(), serde_json::Value::Null)?)
    }

    async fn apply(self, context: &Context) -> anyhow::Result<()> {
        let registry = context
            .get(INVARIANTS)
            .ok_or_else(|| anyhow::anyhow!("{} requires ctx.invariants", self.plugin_name))?;
        let registration = self.register(&registry)?;
        registration.await_ready().await?;
        let cleanup = registration.clone();
        let effect = EffectHandle::new(format!("{}.apply()", self.plugin_name), move || {
            Box::pin(async move { cleanup.dispose().await })
        });
        if let Err(error) = context.own(effect) {
            let cleanup = registration.dispose().await;
            return match cleanup {
                Ok(()) => Err(error.into()),
                Err(cleanup) => Err(anyhow::anyhow!("{error}: rollback failed: {cleanup:#}")),
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::{InvariantConfig, InvariantRegistry};

    use super::*;

    #[tokio::test]
    async fn catalog_metadata_is_unique_and_every_plugin_unwinds_its_exact_reservation() {
        assert_eq!(NOOP_INVARIANTS.len(), 97);
        let mut sources = HashSet::new();
        let mut plugins = HashSet::new();
        let mut packages = HashSet::new();
        for descriptor in NOOP_INVARIANTS {
            assert!(sources.insert(descriptor.source_surface()));
            assert!(plugins.insert(descriptor.plugin_name()));
            assert!(packages.insert(descriptor.package_name()));
            assert!(descriptor.source_surface().ends_with("/invariant.ts"));
            assert!(!descriptor.plugin_name().contains("dsh"));
            assert!(
                descriptor
                    .package_name()
                    .starts_with("@seekdeep-ai/seekdeep-")
            );
        }

        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let mut fibers = Vec::with_capacity(NOOP_INVARIANTS.len());
        for descriptor in NOOP_INVARIANTS {
            let fiber = descriptor.install(&context).unwrap();
            fiber.await_settled().await.unwrap();
            assert!(registry.is_registered(descriptor.package_name()));
            fibers.push((descriptor, fiber));
        }
        while let Some((descriptor, fiber)) = fibers.pop() {
            fiber.dispose().await.unwrap();
            assert!(!registry.is_registered(descriptor.package_name()));
        }
    }
}
