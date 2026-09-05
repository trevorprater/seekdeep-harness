//! Agent-scoped durable one-shot and fixed-rate reminders over the session event log.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, AgentEvent, AgentLifecycleEvent, AgentStatus};
use seekdeep_agent_loop::AgentStatusChanged;
use seekdeep_cordis::{Context, EventOptions, EventReply, Fiber, Plugin, fiber::EffectHandle};
use seekdeep_tools::{TOOLS, ToolRestriction};

use crate::{runtime::ScheduleRuntime, tools::register_schedule_tools};

/// Cordis function-plugin name.
pub const NAME: &str = "schedule";

/// Services required before future root agents can receive Schedule.
pub const INJECT: &[&str] = &["agents", "sessions", "tools", "sessionPersistence"];

/// Installs Schedule only for root agents published after this plugin loads.
///
/// # Errors
///
/// Returns listener-registration or ownership failures.
#[allow(clippy::too_many_lines)]
pub fn apply(ctx: &Context) -> anyhow::Result<()> {
    let runtimes: Arc<Mutex<HashMap<usize, EffectHandle>>> = Arc::new(Mutex::new(HashMap::new()));
    let stopping = Arc::new(AtomicBool::new(false));

    let created_listener = {
        let runtimes = runtimes.clone();
        let stopping = stopping.clone();
        let root = ctx.clone();
        ctx.events().on_sync(
            ctx,
            "agent/created",
            move |_, args| {
                let event = args
                    .get::<AgentLifecycleEvent>(0)
                    .ok_or_else(|| anyhow::anyhow!("agent/created lacks its payload"))?;
                let agent = event.agent.clone();
                let key = Arc::as_ptr(&event.agent) as usize;
                if stopping.load(Ordering::Acquire) || runtimes.lock().contains_key(&key) {
                    return Ok(EventReply::Undefined);
                }
                let is_root = root.get(AGENTS).is_some_and(|registry| {
                    registry
                        .roots()
                        .iter()
                        .any(|root_agent| Arc::ptr_eq(root_agent, &event.agent))
                });
                if !is_root {
                    if let Some(tools) = root.get(TOOLS) {
                        let names = ["schedule_create", "schedule_list", "schedule_delete"];
                        if names
                            .iter()
                            .any(|name| tools.get(name, Some(agent.scope_key())).is_some())
                        {
                            let fiber = Fiber::active_child("schedule child restriction");
                            let scoped = agent.context().with_fiber(fiber.clone());
                            tools.restrict(
                                &scoped,
                                ToolRestriction {
                                    allow: None,
                                    deny: Some(names.iter().map(ToString::to_string).collect()),
                                },
                            )?;
                            let cleanup = EffectHandle::new("schedule.child-restriction()", {
                                let runtimes = runtimes.clone();
                                move || {
                                    Box::pin(async move {
                                        fiber.dispose().await?;
                                        runtimes.lock().remove(&key);
                                        Ok(())
                                    })
                                }
                            });
                            let owned = agent.context().own(cleanup.clone())?;
                            runtimes.lock().insert(key, owned);
                        }
                    }
                    return Ok(EventReply::Undefined);
                }

                let runtime = ScheduleRuntime::new(&root, agent.clone());
                let runtime_drive = runtime.clone();
                let fiber = Fiber::active_child("schedule agent runtime");
                let agent_context = agent.context().with_fiber(fiber.clone());
                let installation = (|| -> anyhow::Result<()> {
                    register_schedule_tools(&root, &agent_context, agent.clone(), move || {
                        runtime_drive.request_drive();
                    })?;
                    let runtime = runtime.clone();
                    let status_agent = agent.clone();
                    agent_context.events().on_sync(
                        &agent_context,
                        "agent/status",
                        move |_, args| {
                            let event = args
                                .get::<AgentEvent<AgentStatusChanged>>(0)
                                .ok_or_else(|| anyhow::anyhow!("agent/status lacks its payload"))?;
                            if Arc::ptr_eq(&event.agent, &status_agent)
                                && event.payload.status == AgentStatus::Idle
                                && status_agent
                                    .session()
                                    .events()
                                    .iter()
                                    .any(|event| event.event_type == "schedule/change")
                            {
                                runtime.request_drive();
                            }
                            Ok(EventReply::Undefined)
                        },
                        EventOptions {
                            global: true,
                            ..EventOptions::default()
                        },
                    )?;
                    Ok(())
                })();
                if let Err(error) = installation {
                    let cleanup = futures::executor::block_on(fiber.dispose());
                    return match cleanup {
                        Ok(()) => Err(error),
                        Err(cleanup) => {
                            Err(anyhow::anyhow!("{error:#}: cleanup failed: {cleanup:#}"))
                        }
                    };
                }
                runtime.start();

                let cleanup = EffectHandle::new("schedule.runtime()", {
                    let runtime = runtime.clone();
                    let runtimes = runtimes.clone();
                    let fiber = fiber.clone();
                    move || {
                        Box::pin(async move {
                            runtime.dispose().await;
                            fiber.dispose().await?;
                            runtimes.lock().remove(&key);
                            Ok(())
                        })
                    }
                });
                let owned = agent.context().own(cleanup.clone())?;
                runtimes.lock().insert(key, owned);
                Ok(EventReply::Undefined)
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )?
    };

    let lifecycle = EffectHandle::new("schedule.lifecycle()", {
        let runtimes = runtimes.clone();
        let stopping = stopping.clone();
        let created_listener = created_listener.clone();
        move || {
            Box::pin(async move {
                stopping.store(true, Ordering::Release);
                let _ = created_listener.dispose().await;
                let cleanups = std::mem::take(&mut *runtimes.lock());
                for cleanup in cleanups.into_values() {
                    let _ = cleanup.dispose().await;
                }
                Ok(())
            })
        }
    });
    ctx.own(lifecycle)?;
    Ok(())
}

/// Builds the Loader-compatible Schedule function plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, _| {
        Box::pin(async move {
            apply(&context)?;
            Ok(())
        })
    })
}
