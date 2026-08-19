//! Agent-scoped durable one-shot and fixed-rate reminders over the session event log.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, AgentLifecycleEvent, AgentStatus};
use seekdeep_agent_loop::AgentStatusChanged;
use seekdeep_cordis::{Context, EventOptions, EventReply, fiber::EffectHandle};

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
                    return Ok(EventReply::Undefined);
                }

                let runtime = ScheduleRuntime::new(&root, agent.clone());
                let runtime_drive = runtime.clone();
                register_schedule_tools(&root, agent.context(), agent.clone(), move || {
                    runtime_drive.request_drive();
                })?;
                {
                    let runtime = runtime.clone();
                    let status_agent = agent.clone();
                    let agent_context = agent.context().clone();
                    agent_context.events().on_sync(
                        &agent_context,
                        "agent/status",
                        move |_, args| {
                            let status = args
                                .get::<AgentStatusChanged>(0)
                                .ok_or_else(|| anyhow::anyhow!("agent/status lacks its payload"))?;
                            if status.status == AgentStatus::Idle
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
                }
                runtime.start();

                let cleanup = EffectHandle::new("schedule.runtime()", {
                    let runtime = runtime.clone();
                    let runtimes = runtimes.clone();
                    move || {
                        Box::pin(async move {
                            runtime.dispose().await;
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
