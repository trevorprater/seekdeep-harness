//! Compiled test-only plugins referenced by ACP snapshot overlays.

use seekdeep_cordis::{EventOptions, EventReply, Plugin};

pub(crate) fn subagent_settlement_marker_plugin() -> Plugin {
    Plugin::new(
        "subagent-settlement-marker",
        std::iter::empty::<&'static str>(),
        |context, _| {
            Box::pin(async move {
                context.events().on_sync(
                    &context,
                    "subagent/end",
                    |_, _| {
                        std::fs::write(
                            std::env::current_dir()?.join(".seekdeep-snapshot-subagent-settled"),
                            "",
                        )?;
                        Ok(EventReply::Undefined)
                    },
                    EventOptions::default(),
                )?;
                Ok(())
            })
        },
    )
}
