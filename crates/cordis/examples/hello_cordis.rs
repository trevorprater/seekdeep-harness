//! Five plugins on one root context: two `greeter` providers, an app that
//! injects `greeter`, and two observers of the `greet` event the app emits.
//!
//! `cargo run -p seekdeep-cordis --example hello_cordis` prints the transcript;
//! `cargo test -p seekdeep-cordis --example hello_cordis` pins it stage by stage.

// On wasm32 only the stub `main` at the bottom is live.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, EventArgs, EventOptions, EventReply, FiberState, Plugin, PluginFiber, ServiceKey,
    fiber::EffectHandle,
};
use serde_json::Value;

/// Slot the app injects; whichever greeter plugin is active provides it.
const GREETER: ServiceKey<Greeter> = ServiceKey::new("greeter");

struct Greeter {
    /// `{name}` stands for the person being greeted.
    template: &'static str,
}

impl Greeter {
    fn greet(&self, name: &str) -> String {
        self.template.replace("{name}", name)
    }
}

/// What the plugins say, in order, so a stage can be printed or asserted.
#[derive(Clone, Default)]
struct Transcript(Arc<Mutex<Vec<String>>>);

impl Transcript {
    fn say(&self, line: impl Into<String>) {
        self.0.lock().push(line.into());
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock())
    }
}

/// Provider: `provide` is an effect of this fiber, so unloading the plugin
/// withdraws the service and wakes every plugin that injects it.
fn greeter_plugin(name: &'static str, template: &'static str) -> Plugin {
    Plugin::new(name, std::iter::empty::<&str>(), move |context, _| {
        Box::pin(async move {
            context.provide(GREETER, Arc::new(Greeter { template }))?;
            Ok(())
        })
    })
}

/// Consumer: `inject` keeps the fiber `Pending` until `greeter` is provided,
/// runs the body once it is, unwinds the body's effects when the provider
/// unloads, and runs the body again for the next provider.
fn app_plugin(transcript: Transcript) -> Plugin {
    Plugin::new("app", ["greeter"], move |context, _| {
        let transcript = transcript.clone();
        Box::pin(async move {
            let greeter = context
                .get(GREETER)
                .ok_or_else(|| anyhow::anyhow!("app ran without a provided greeter"))?;
            transcript.say(format!("[App] {}", greeter.greet("Alex")));
            context
                .events()
                .emit(&context, "greet", &EventArgs::one(String::from("Alex")))?;
            let farewell = transcript.clone();
            context.own(EffectHandle::synchronous("app deactivation", move || {
                farewell.say("[App] deactivated (greeter unavailable)");
                Ok(())
            }))?;
            Ok(())
        })
    })
}

/// Observer: the listener is an effect of this fiber, and the observer never
/// learns which plugin emitted `greet`.
fn observer_plugin(
    name: &'static str,
    transcript: Transcript,
    render: fn(&str) -> String,
) -> Plugin {
    Plugin::new(name, std::iter::empty::<&str>(), move |context, _| {
        let transcript = transcript.clone();
        Box::pin(async move {
            context.events().on_sync(
                &context,
                "greet",
                move |_, args| {
                    let who = args
                        .get::<String>(0)
                        .ok_or_else(|| anyhow::anyhow!("greet was emitted without a name"))?;
                    transcript.say(render(&who));
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?;
            Ok(())
        })
    })
}

/// One step of the scenario, observed after every fiber settled.
struct Stage {
    title: &'static str,
    said: Vec<String>,
    app: FiberState,
}

async fn settle(
    context: &Context,
    app: &PluginFiber,
    transcript: &Transcript,
    title: &'static str,
) -> Stage {
    context.registry().await_quiescent().await;
    Stage {
        title,
        said: transcript.take(),
        app: app.fiber().state(),
    }
}

async fn run() -> anyhow::Result<Vec<Stage>> {
    let transcript = Transcript::default();
    let context = Context::new();

    context.plugin(
        observer_plugin("logger", transcript.clone(), |who| {
            format!("[LOG] greeted {who}")
        }),
        Value::Null,
    )?;
    context.plugin(
        observer_plugin("analytics", transcript.clone(), |who| {
            format!("[ANALYTICS] {who} said hi")
        }),
        Value::Null,
    )?;
    let app = context.plugin(app_plugin(transcript.clone()), Value::Null)?;
    let mut stages = vec![
        settle(
            &context,
            &app,
            &transcript,
            "logger, analytics, and app mounted",
        )
        .await,
    ];

    let english = context.plugin(
        greeter_plugin("english-greeter", "Hello, {name}!"),
        Value::Null,
    )?;
    stages.push(settle(&context, &app, &transcript, "english-greeter mounted").await);

    english.dispose().await?;
    stages.push(settle(&context, &app, &transcript, "english-greeter disposed").await);

    context.plugin(
        greeter_plugin("chinese-greeter", "你好，{name}！"),
        Value::Null,
    )?;
    stages.push(settle(&context, &app, &transcript, "chinese-greeter mounted").await);

    Ok(stages)
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    for stage in run().await? {
        println!("=== {} ===", stage.title);
        for line in &stage.said {
            println!("{line}");
        }
        println!("app is {:?}", stage.app);
    }
    Ok(())
}

/// The browser build has no runtime to drive the walkthrough.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn app_follows_the_greeter_provider() {
        let stages = run().await.expect("scenario runs");
        let observed = stages
            .iter()
            .map(|stage| {
                (
                    stage.title,
                    stage.said.iter().map(String::as_str).collect::<Vec<_>>(),
                    stage.app,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            [
                (
                    "logger, analytics, and app mounted",
                    vec![],
                    FiberState::Pending
                ),
                (
                    "english-greeter mounted",
                    vec![
                        "[App] Hello, Alex!",
                        "[LOG] greeted Alex",
                        "[ANALYTICS] Alex said hi",
                    ],
                    FiberState::Active
                ),
                (
                    "english-greeter disposed",
                    vec!["[App] deactivated (greeter unavailable)"],
                    FiberState::Pending
                ),
                (
                    "chinese-greeter mounted",
                    vec![
                        "[App] 你好，Alex！",
                        "[LOG] greeted Alex",
                        "[ANALYTICS] Alex said hi",
                    ],
                    FiberState::Active
                ),
            ]
        );
    }
}
