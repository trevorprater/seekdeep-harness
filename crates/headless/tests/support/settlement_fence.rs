//! Source fixture ordering for the parent request after background-child admission.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use seekdeep_agent::AgentEvent;
use seekdeep_agent_loop::{AgentInboxMessage, AgentPreStepEvent};
use seekdeep_cordis::{EventOptions, EventReply, Plugin};
use tokio::sync::Notify;

pub(super) fn plugin() -> Plugin {
    Plugin::new(
        "subagent-settlement-fence",
        std::iter::empty::<&str>(),
        |context, _| {
            Box::pin(async move {
                let delivered = Arc::new(Notify::new());
                let has_delivered = Arc::new(AtomicBool::new(false));
                let inbox_delivered = delivered.clone();
                let inbox_has_delivered = has_delivered.clone();
                context.events().on_sync(
                    &context,
                    "agent/inbox/inserted",
                    move |_, args| {
                        let event = args.get::<AgentEvent<AgentInboxMessage>>(0).unwrap();
                        if event.agent.session().header().parent_session.is_none()
                            && event.payload.message.source().kind == "subagent-settled"
                        {
                            inbox_has_delivered.store(true, Ordering::Release);
                            inbox_delivered.notify_waiters();
                        }
                        Ok(EventReply::Undefined)
                    },
                    EventOptions {
                        global: true,
                        prepend: false,
                    },
                )?;
                context.events().on_waterfall(
                    &context,
                    "agent/pre-step",
                    move |_, args, next| {
                        let delivered = delivered.clone();
                        let has_delivered = has_delivered.clone();
                        Box::pin(async move {
                            let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                            if event.agent.session().header().parent_session.is_none()
                                && event.payload.turn == 1
                                && event.payload.step == 2
                            {
                                loop {
                                    let notice = delivered.notified();
                                    if has_delivered.load(Ordering::Acquire) {
                                        break;
                                    }
                                    notice.await;
                                }
                            }
                            next.run().await
                        })
                    },
                    EventOptions {
                        global: true,
                        prepend: false,
                    },
                )?;
                Ok(())
            })
        },
    )
}
