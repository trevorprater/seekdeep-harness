//! Reflected provider, accessor, mixin, conflict, and reversal source oracle.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, DynamicValue, MixinMember, ServiceKey};

const ACCESSOR_CONFLICT: ServiceKey<i32> = ServiceKey::new("accessorConflict");
const SERVICE_CONFLICT: ServiceKey<i32> = ServiceKey::new("serviceConflict");

#[test]
fn computed_accessors_read_write_reject_read_only_sets_and_reverse_exactly() {
    let context = Context::new();
    let state = Arc::new(Mutex::new(1_i32));
    let read_state = state.clone();
    let write_state = state.clone();
    let virtual_property = context
        .accessor(
            "virtual",
            move |_| Ok(Some(Arc::new(*read_state.lock()))),
            Some(move |_context: &Context, value: Arc<i32>| {
                *write_state.lock() = *value;
                Ok(true)
            }),
        )
        .expect("virtual accessor");
    assert!(context.has_property("virtual"));
    assert_eq!(
        context.property::<i32>("virtual").unwrap().as_deref(),
        Some(&1)
    );
    assert!(context.get_named::<i32>("virtual").is_none());
    assert!(
        context
            .set_property("virtual", Arc::new(4_i32))
            .expect("set virtual")
    );
    assert_eq!(*state.lock(), 4);
    assert_eq!(
        context.property::<i32>("virtual").unwrap().as_deref(),
        Some(&4)
    );

    let read_only = context
        .accessor_read_only("readOnly", |_| Ok(Some(Arc::new("fixed".to_owned()))))
        .expect("read-only accessor");
    assert_eq!(
        context
            .property::<String>("readOnly")
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("fixed")
    );
    assert!(
        !context
            .set_property("readOnly", Arc::new("x".to_owned()))
            .unwrap()
    );

    futures::executor::block_on(read_only.dispose()).unwrap();
    futures::executor::block_on(virtual_property.dispose()).unwrap();
    assert!(!context.has_property("virtual"));
    assert!(context.property::<i32>("virtual").unwrap().is_none());
}

#[tokio::test]
async fn accessor_service_declarations_conflict_even_after_provider_withdrawal() {
    let context = Context::new();
    let accessor = context
        .accessor_read_only("accessorConflict", |_| Ok(Some(Arc::new(1_i32))))
        .expect("accessor");
    let error = context
        .provide(ACCESSOR_CONFLICT, Arc::new(2_i32))
        .expect_err("service must conflict");
    assert_eq!(
        error.to_string(),
        "property \"accessorConflict\" is already declared as accessor"
    );

    let service = context
        .provide(SERVICE_CONFLICT, Arc::new(1_i32))
        .expect("service");
    service.dispose().await.unwrap();
    assert!(context.has_property("serviceConflict"));
    let error = context
        .accessor_read_only("serviceConflict", |_| Ok(Some(Arc::new(2_i32))))
        .expect_err("accessor must conflict");
    assert_eq!(
        error.to_string(),
        "property \"serviceConflict\" is already declared as service"
    );
    accessor.dispose().await.unwrap();
}

#[derive(Debug)]
struct Math {
    value: Arc<Mutex<i32>>,
}

#[derive(Debug)]
struct Adder {
    value: Arc<Mutex<i32>>,
}

impl Adder {
    fn call(&self, operand: i32) -> i32 {
        *self.value.lock() + operand
    }
}

const MATH: ServiceKey<Math> = ServiceKey::new("math");

#[tokio::test]
async fn mixin_forwards_bound_values_and_methods_then_disposes_as_one_effect() {
    let context = Context::new();
    let value = Arc::new(Mutex::new(2_i32));
    let service = context
        .provide(
            MATH,
            Arc::new(Math {
                value: value.clone(),
            }),
        )
        .expect("math service");
    let mixin = context
        .mixin(
            MATH,
            [
                MixinMember::read_only("value", |math: &Math| Arc::new(*math.value.lock()))
                    .with_setter(|math, value: DynamicValue| {
                        let value = Arc::downcast::<i32>(value)
                            .map_err(|_| anyhow::anyhow!("expected i32"))?;
                        *math.value.lock() = *value;
                        Ok(true)
                    }),
                MixinMember::read_only("add", |math: &Math| {
                    Arc::new(Adder {
                        value: math.value.clone(),
                    })
                }),
            ],
        )
        .expect("mixin");
    assert!(context.has_property("add"));
    assert_eq!(
        context.property::<i32>("value").unwrap().as_deref(),
        Some(&2)
    );
    assert_eq!(
        context.property::<Adder>("add").unwrap().unwrap().call(3),
        5
    );
    assert!(context.set_property("value", Arc::new(7_i32)).unwrap());
    assert_eq!(*value.lock(), 7);
    assert_eq!(
        context.property::<Adder>("add").unwrap().unwrap().call(3),
        10
    );

    mixin.dispose().await.unwrap();
    assert!(!context.has_property("add"));
    assert!(context.property::<Adder>("add").unwrap().is_none());
    service.dispose().await.unwrap();
}
