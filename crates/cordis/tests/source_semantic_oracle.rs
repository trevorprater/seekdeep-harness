//! Semantic conformance scenarios pinned from the source Cordis runtime.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use parking_lot::Mutex;
use seekdeep_cordis::{
    BailReply, Context, EventArgs, EventOptions, EventReply, Fiber, FiberState, Plugin, ServiceKey,
    fiber::EffectHandle,
};
use serde_json::{Value, json};

#[derive(Debug, PartialEq, Eq)]
struct ServiceValue(&'static str);

const SERVICE: ServiceKey<ServiceValue> = ServiceKey::new("svc");
const MISSING: ServiceKey<ServiceValue> = ServiceKey::new("missing");

#[test]
fn context_extension_isolation_intercepts_and_provider_owned_set_match_the_oracle() {
    let root = Context::new();
    assert_eq!(root.fiber().name(), "root");
    assert_eq!(root.fiber().state(), FiberState::Active);
    assert_eq!(root.root_fiber().state(), FiberState::Active);

    let child = root.with_meta("marker", json!("child"));
    assert_eq!(child.meta("marker"), Some(json!("child")));
    assert_eq!(root.meta("marker"), None);

    root.provide(SERVICE, Arc::new(ServiceValue("root")))
        .expect("root provider");
    let isolated_a = root.isolate_named_as("svc", "shared");
    let isolated_b = root.isolate_named_as("svc", "shared");
    let isolated_c = root.isolate_named("svc");
    assert_eq!(root.get(SERVICE).as_deref(), Some(&ServiceValue("root")));
    assert!(isolated_a.get(SERVICE).is_none());
    assert!(isolated_c.get(SERVICE).is_none());
    isolated_a
        .provide(SERVICE, Arc::new(ServiceValue("isolated")))
        .expect("isolated provider");
    assert_eq!(
        isolated_a.get(SERVICE).as_deref(),
        Some(&ServiceValue("isolated"))
    );
    assert_eq!(
        isolated_b.get(SERVICE).as_deref(),
        Some(&ServiceValue("isolated"))
    );
    assert!(isolated_c.get(SERVICE).is_none());
    assert_eq!(root.get(SERVICE).as_deref(), Some(&ServiceValue("root")));

    let intercepted = root
        .intercept("probe", json!({"root":1, "nested":{"a":1}}))
        .intercept("probe", json!({"child":2, "nested":{"b":2}}));
    assert_eq!(
        intercepted.resolve_intercepted(
            "probe",
            Some(&json!({"base":0, "nested":{"base":true}})),
            Some(&json!({"head":3})),
        ),
        json!({"base":0, "nested":{"b":2}, "root":1, "child":2, "head":3})
    );
    assert_eq!(
        intercepted.resolve_intercepted_with("probe", None, None, |layers| { json!(layers.len()) }),
        json!(2)
    );

    root.set(SERVICE, Arc::new(ServiceValue("replaced")))
        .expect("provider-owned set");
    assert_eq!(
        root.get(SERVICE).as_deref(),
        Some(&ServiceValue("replaced"))
    );
    let foreign_fiber = Fiber::active_child("foreign");
    let foreign = root.with_fiber(foreign_fiber);
    assert!(
        foreign
            .set(SERVICE, Arc::new(ServiceValue("forbidden")))
            .unwrap_err()
            .to_string()
            .contains("multiple fibers")
    );
    assert!(
        root.set(MISSING, Arc::new(ServiceValue("missing")))
            .unwrap_err()
            .to_string()
            .contains("without provide")
    );
}

#[test]
fn event_order_once_and_bail_match_the_oracle() {
    let context = Context::new();
    let order = Arc::new(Mutex::new(Vec::new()));
    for (label, options) in [
        ("normal", EventOptions::default()),
        (
            "prepend",
            EventOptions {
                prepend: true,
                global: false,
            },
        ),
    ] {
        let order = order.clone();
        context
            .events()
            .on_sync(
                &context,
                "order",
                move |_, _| {
                    order.lock().push(label);
                    Ok(EventReply::Undefined)
                },
                options,
            )
            .expect("listener");
    }
    let once_order = order.clone();
    context
        .events()
        .once_sync(
            &context,
            "order",
            move |_, _| {
                once_order.lock().push("once");
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("once");
    context
        .events()
        .emit(&context, "order", &EventArgs::new())
        .expect("first emit");
    context
        .events()
        .emit(&context, "order", &EventArgs::new())
        .expect("second emit");
    assert_eq!(
        *order.lock(),
        ["prepend", "normal", "once", "prepend", "normal"]
    );

    let bail_order = Arc::new(Mutex::new(Vec::new()));
    for (label, reply) in [
        ("false", EventReply::False),
        ("null", EventReply::Null),
        ("zero", EventReply::Value(Arc::new(0_i32))),
        ("late", EventReply::Value(Arc::new("late".to_owned()))),
    ] {
        let bail_order = bail_order.clone();
        context
            .events()
            .on_sync(
                &context,
                "bail",
                move |_, _| {
                    bail_order.lock().push(label);
                    Ok(reply.clone())
                },
                EventOptions::default(),
            )
            .expect("bail listener");
    }
    let BailReply::Settled(reply) = context
        .events()
        .bail(&context, "bail", &EventArgs::new())
        .expect("bail")
    else {
        panic!("synchronous listeners must settle")
    };
    assert_eq!(*reply.downcast::<i32>().expect("zero"), 0);
    assert_eq!(*bail_order.lock(), ["false", "null", "zero"]);
}

#[tokio::test]
async fn serial_waterfall_filter_and_parallel_errors_match_the_oracle() {
    let context = Context::new();
    let serial_order = Arc::new(Mutex::new(Vec::new()));
    for (label, reply) in [
        ("a", EventReply::Undefined),
        ("b", EventReply::Value(Arc::new("stop".to_owned()))),
        ("c", EventReply::Value(Arc::new("late".to_owned()))),
    ] {
        let serial_order = serial_order.clone();
        context
            .events()
            .on(
                &context,
                "serial",
                move |_, _| {
                    let serial_order = serial_order.clone();
                    let reply = reply.clone();
                    Box::pin(async move {
                        serial_order.lock().push(label);
                        Ok(reply)
                    })
                },
                EventOptions::default(),
            )
            .expect("serial listener");
    }
    let serial = context
        .events()
        .serial(&context, "serial", &EventArgs::new())
        .await
        .expect("serial");
    assert_eq!(
        serial.downcast::<String>().as_deref().map(String::as_str),
        Some("stop")
    );
    assert_eq!(*serial_order.lock(), ["a", "b"]);

    let waterfall_order = Arc::new(Mutex::new(Vec::new()));
    for label in ["outer", "inner"] {
        let waterfall_order = waterfall_order.clone();
        context
            .events()
            .on_waterfall(
                &context,
                "waterfall",
                move |_, _, next| {
                    let waterfall_order = waterfall_order.clone();
                    Box::pin(async move {
                        waterfall_order.lock().push(format!("{label}-before"));
                        let answer = next.run().await?.downcast::<i32>().expect("number");
                        waterfall_order
                            .lock()
                            .push(format!("{label}-after:{answer}"));
                        Ok(EventReply::Value(Arc::new(*answer + 1)))
                    })
                },
                EventOptions::default(),
            )
            .expect("waterfall listener");
    }
    let core_order = waterfall_order.clone();
    let waterfall = context
        .events()
        .waterfall(&context, "waterfall", &EventArgs::one(4_i32), move || {
            Box::pin(async move {
                core_order.lock().push("core".to_owned());
                Ok(EventReply::Value(Arc::new(10_i32)))
            })
        })
        .await
        .expect("waterfall");
    assert_eq!(*waterfall.downcast::<i32>().expect("answer"), 12);
    assert_eq!(
        *waterfall_order.lock(),
        [
            "outer-before",
            "inner-before",
            "core",
            "inner-after:10",
            "outer-after:11"
        ]
    );

    assert_filter_and_parallel_errors(&context).await;
}

async fn assert_filter_and_parallel_errors(context: &Context) {
    let filtered = Arc::new(Mutex::new(Vec::new()));
    for (scope, global) in [("allowed", false), ("denied", false), ("global", true)] {
        let owner = context.with_meta("scope", json!(scope));
        let filtered = filtered.clone();
        context
            .events()
            .on_sync(
                &owner,
                "filtered",
                move |_, _| {
                    filtered.lock().push(scope);
                    Ok(EventReply::Undefined)
                },
                EventOptions {
                    prepend: false,
                    global,
                },
            )
            .expect("filtered listener");
    }
    let dispatch = context.with_event_filter(|listener| {
        listener.meta("scope").as_ref().and_then(Value::as_str) == Some("allowed")
    });
    context
        .events()
        .emit(&dispatch, "filtered", &EventArgs::new())
        .expect("filtered emit");
    assert_eq!(*filtered.lock(), ["allowed", "global"]);

    let completion = Arc::new(tokio::sync::Notify::new());
    let parallel_order = Arc::new(Mutex::new(Vec::new()));
    let slow_completion = completion.clone();
    let slow_order = parallel_order.clone();
    context
        .events()
        .on(
            context,
            "parallel",
            move |_, _| {
                let completion = slow_completion.clone();
                let order = slow_order.clone();
                Box::pin(async move {
                    completion.notified().await;
                    order.lock().push("slow");
                    anyhow::bail!("slow-error")
                })
            },
            EventOptions::default(),
        )
        .expect("slow listener");
    let fast_order = parallel_order.clone();
    context
        .events()
        .on(
            context,
            "parallel",
            move |_, _| {
                let completion = completion.clone();
                let order = fast_order.clone();
                Box::pin(async move {
                    order.lock().push("fast");
                    completion.notify_one();
                    anyhow::bail!("fast-error")
                })
            },
            EventOptions::default(),
        )
        .expect("fast listener");
    let error = context
        .events()
        .parallel(context, "parallel", &EventArgs::new())
        .await
        .expect_err("parallel aggregate");
    assert_eq!(*parallel_order.lock(), ["fast", "slow"]);
    let message = error.to_string();
    assert!(message.starts_with("slow-error\nfast-error"), "{message}");
}

#[derive(Debug, PartialEq)]
struct Made(Value);

const DEP: ServiceKey<Value> = ServiceKey::new("dep");
const MADE: ServiceKey<Made> = ServiceKey::new("made");

#[tokio::test]
async fn dependency_epoch_activation_inject_intercepts_and_disposal_match_the_oracle() {
    let context = Context::new().intercept("dep", json!({"root":1, "nested":{"a":1}}));
    let starts = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let observed_intercept = Arc::new(Mutex::new(None));
    let plugin = Plugin::new("dependent", ["dep"], {
        let starts = starts.clone();
        let stops = stops.clone();
        let observed_intercept = observed_intercept.clone();
        move |context, config| {
            let starts = starts.clone();
            let stops = stops.clone();
            let observed_intercept = observed_intercept.clone();
            Box::pin(async move {
                starts.fetch_add(1, Ordering::AcqRel);
                *observed_intercept.lock() = Some(context.resolve_intercepted(
                    "dep",
                    Some(&json!({"base":0})),
                    Some(&json!({"head":3})),
                ));
                context.provide(MADE, Arc::new(Made(config)))?;
                context.own(EffectHandle::synchronous("count stop", move || {
                    stops.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                }))?;
                Ok(())
            })
        }
    })
    .with_inject_config("dep", json!({"child":2, "nested":{"b":2}}));
    let fiber = context.plugin(plugin, json!({"value":1})).expect("plugin");
    fiber.await_settled().await.expect("pending settled");
    assert_eq!(fiber.fiber().state(), FiberState::Pending);
    assert_eq!(starts.load(Ordering::Acquire), 0);
    assert!(context.get(MADE).is_none());

    let dependency = context
        .provide(DEP, Arc::new(json!({"generation":1})))
        .expect("dependency");
    fiber.await_settled().await.expect("active");
    assert_eq!(fiber.fiber().state(), FiberState::Active);
    assert_eq!(starts.load(Ordering::Acquire), 1);
    assert_eq!(
        context.get(MADE).as_deref(),
        Some(&Made(json!({"value":1})))
    );
    assert_eq!(
        *observed_intercept.lock(),
        Some(json!({"base":0, "root":1, "child":2, "nested":{"b":2}, "head":3}))
    );

    dependency.dispose().await.expect("withdraw dependency");
    fiber.await_settled().await.expect("pending again");
    assert_eq!(fiber.fiber().state(), FiberState::Pending);
    assert_eq!(stops.load(Ordering::Acquire), 1);
    assert!(context.get(MADE).is_none());

    fiber.dispose().await.expect("dispose plugin");
    assert_eq!(fiber.fiber().state(), FiberState::Disposed);
    assert_eq!(starts.load(Ordering::Acquire), 1);
    assert_eq!(stops.load(Ordering::Acquire), 1);
}
