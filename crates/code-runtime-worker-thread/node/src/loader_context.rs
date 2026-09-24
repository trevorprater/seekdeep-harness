//! Host Context effects projected into the catalog's native Node realm.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use js_sys::Array;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::{
    bridge,
    loader::{Realm, emit, settled, wire},
};

#[wasm_bindgen(inline_js = r"
const generator = Object.getPrototypeOf(function*() {}).constructor;
const asyncGenerator = Object.getPrototypeOf(async function*() {}).constructor;
const symbolFor = Symbol.for;
export function loaderIsGenerator(value) { return value instanceof generator || value instanceof asyncGenerator; }
export function loaderSymbol(name) { return symbolFor(name); }
")]
extern "C" {
    #[wasm_bindgen(js_name = loaderIsGenerator)]
    fn is_generator(value: &JsValue) -> bool;
    #[wasm_bindgen(js_name = loaderSymbol)]
    fn symbol(name: &str) -> JsValue;
}

pub(crate) struct Activation {
    pub(crate) id: u64,
    pub(crate) module: u64,
    pub(crate) native_key: String,
    pub(crate) fiber: JsValue,
    config: JsValue,
    context: JsValue,
    callbacks: BTreeMap<u64, JsValue>,
    disposers: Vec<JsValue>,
    pending: Vec<JsValue>,
    retained: Vec<JsValue>,
    services: Vec<u64>,
    next_effect: u64,
    disposed: bool,
    failed: bool,
    pub(crate) tools: BTreeMap<u64, JsValue>,
}

pub(crate) fn callback(
    function: impl FnMut(Array) -> Result<JsValue, JsValue> + 'static,
) -> JsValue {
    let callback =
        Closure::<dyn FnMut(Array) -> Result<JsValue, JsValue>>::new(function).into_js_value();
    bridge::variadic(&callback).into()
}

fn set(object: &JsValue, key: &str, value: &JsValue) -> Result<(), JsValue> {
    bridge::define(object, &JsValue::from_str(key), value, true, true)
}

fn own(activation: &Rc<RefCell<Activation>>, disposer: JsValue) -> JsValue {
    let mut active = true;
    let owned = callback(move |_| {
        if !active {
            return Ok(JsValue::UNDEFINED);
        }
        active = false;
        bridge::apply(&disposer, &JsValue::UNDEFINED, &[])
    });
    activation.borrow_mut().disposers.push(owned.clone());
    owned
}

pub(crate) fn call(
    realm: &Rc<RefCell<Realm>>,
    activation: &Rc<RefCell<Activation>>,
    mut command: Value,
) -> Result<JsValue, JsValue> {
    if activation.borrow().disposed
        && !matches!(
            command["type"].as_str(),
            Some("disposeEffect" | "log" | "event")
        )
    {
        return Err(bridge::error("Context has been disposed"));
    }
    let (call, id) = {
        let mut realm = realm.borrow_mut();
        let call = realm.next_call;
        realm.next_call += 1;
        (call, activation.borrow().id)
    };
    command["activation"] = json!(id);
    command["call"] = json!(call);
    let owner = realm.clone();
    let executor = Closure::once_into_js(move |resolve: JsValue, reject: JsValue| {
        owner.borrow_mut().pending.insert(call, (resolve, reject));
    });
    let promise = bridge::promise(&executor);
    emit(realm, &command)?;
    // Install a rejection handler immediately; native apply awaits every admitted effect.
    let ignored = callback(|_| Ok(JsValue::UNDEFINED));
    let _ = bridge::then(&promise, &JsValue::UNDEFINED, &ignored)?;
    if !matches!(command["type"].as_str(), Some("event" | "disposeRoot")) {
        activation.borrow_mut().pending.push(promise.clone());
    }
    Ok(promise)
}

pub(crate) async fn drain(activation: &Rc<RefCell<Activation>>) -> Result<(), JsValue> {
    loop {
        let pending = std::mem::take(&mut activation.borrow_mut().pending);
        if pending.is_empty() {
            return Ok(());
        }
        for promise in pending {
            settled(promise).await?;
        }
    }
}

pub(crate) fn command_disposer(
    realm: &Rc<RefCell<Realm>>,
    activation: &Rc<RefCell<Activation>>,
    effect: u64,
) -> JsValue {
    let realm = realm.clone();
    let activation = activation.clone();
    let mut active = true;
    callback(move |_| {
        if !active {
            return Ok(JsValue::UNDEFINED);
        }
        active = false;
        call(
            &realm,
            &activation,
            json!({"type":"disposeEffect","effect":effect}),
        )
    })
}

pub(crate) fn next_effect(activation: &Rc<RefCell<Activation>>) -> u64 {
    let mut activation = activation.borrow_mut();
    let effect = activation.next_effect;
    activation.next_effect += 1;
    effect
}

fn provide(
    realm: &Rc<RefCell<Realm>>,
    activation: &Rc<RefCell<Activation>>,
    args: &Array,
) -> Result<JsValue, JsValue> {
    let name = bridge::string(&args.get(0))?;
    let value = args.get(1);
    let effect = next_effect(activation);
    let context = activation.borrow().context.clone();
    set(&context, &name, &value)?;
    let mut methods = Vec::new();
    let mut projection = serde_json::Map::new();
    if value.is_object() && !value.is_null() || value.is_function() {
        let mut current = value.clone();
        while !current.is_null() {
            for key in bridge::own_keys(&current)?.iter() {
                let Some(name) = key.as_string() else {
                    continue;
                };
                if name == "constructor" {
                    continue;
                }
                let field = bridge::get_key(&value, &key)?;
                if field.is_function() {
                    if !methods.contains(&name) {
                        methods.push(name);
                    }
                } else if let Ok(field) = wire(&field) {
                    projection.insert(name, field);
                }
            }
            if bridge::is_array(&value) {
                break;
            }
            current = bridge::prototype(&current)?;
            if current
                == bridge::get(&bridge::api("global")?, "Object")
                    .and_then(|object| bridge::get(&object, "prototype"))?
            {
                break;
            }
        }
    }
    let service = {
        let mut realm = realm.borrow_mut();
        let id = realm.next_service;
        realm.next_service += 1;
        realm.services.insert(id, value.clone());
        id
    };
    activation.borrow_mut().services.push(service);
    let command = if methods.is_empty() {
        json!({"type":"provide","effect":effect,"name":name,"service":service,"value":wire(&value).unwrap_or(Value::Null)})
    } else {
        json!({"type":"provideDynamic","effect":effect,"name":name,"service":service,"methods":methods,"projection":projection})
    };
    call(realm, activation, command)?;
    Ok(command_disposer(realm, activation, effect))
}

fn listen(
    realm: &Rc<RefCell<Realm>>,
    activation: &Rc<RefCell<Activation>>,
    args: &Array,
    once: bool,
) -> Result<JsValue, JsValue> {
    let name = bridge::string(&args.get(0))?;
    let callback = args.get(1);
    if !callback.is_function() {
        return Err(bridge::error("event listener must be a function"));
    }
    let effect = next_effect(activation);
    activation.borrow_mut().callbacks.insert(effect, callback);
    call(
        realm,
        activation,
        json!({"type":"on","effect":effect,"name":name,"once":once}),
    )?;
    Ok(command_disposer(realm, activation, effect))
}

fn timer(
    activation: &Rc<RefCell<Activation>>,
    args: &Array,
    repeat: bool,
) -> Result<JsValue, JsValue> {
    let global = bridge::api("global")?;
    let function = args.get(0);
    if !function.is_function() {
        if repeat {
            return Err(bridge::error("interval requires a callback"));
        }
        let delay = function;
        let owner = activation.clone();
        let executor = Closure::once_into_js(move |resolve: JsValue, reject: JsValue| {
            let timer = bridge::method(&global, "setTimeout", &[resolve, delay]);
            if let Ok(timer) = timer {
                let disposer = callback(move |_| {
                    bridge::method(&global, "clearTimeout", std::slice::from_ref(&timer))?;
                    bridge::apply(
                        &reject,
                        &JsValue::UNDEFINED,
                        &[bridge::error("Context has been disposed")],
                    )
                });
                own(&owner, disposer);
            }
        });
        return Ok(bridge::promise(&executor));
    }
    let timer = bridge::method(
        &global,
        if repeat { "setInterval" } else { "setTimeout" },
        &[function, args.get(1)],
    )?;
    let disposer = callback(move |_| {
        bridge::method(
            &global,
            if repeat {
                "clearInterval"
            } else {
                "clearTimeout"
            },
            std::slice::from_ref(&timer),
        )
    });
    Ok(own(activation, disposer))
}

fn context(
    realm: &Rc<RefCell<Realm>>,
    activation: &Rc<RefCell<Activation>>,
    request: &Value,
) -> Result<JsValue, JsValue> {
    let ctx = bridge::from_json(&request["services"])?;
    activation.borrow_mut().context = ctx.clone();
    if let Some(services) = request["dynamic"].as_object() {
        for (name, descriptor) in services {
            if let Some(service) = descriptor["service"]
                .as_u64()
                .and_then(|id| realm.borrow().services.get(&id).cloned())
            {
                set(&ctx, name, &service)?;
            }
        }
    }
    for name in [
        "provide",
        "on",
        "once",
        "effect",
        "get",
        "emit",
        "parallel",
        "serial",
        "bail",
        "timeout",
        "interval",
        "setTimeout",
        "setInterval",
    ] {
        let realm = realm.clone();
        let activation = activation.clone();
        let function = callback(move |args| match name {
            "provide" => provide(&realm, &activation, &args),
            "on" | "once" => listen(&realm, &activation, &args, name == "once"),
            "effect" => {
                let disposer = bridge::apply(&args.get(0), &JsValue::UNDEFINED, &[])?;
                Ok(if disposer.is_function() {
                    own(&activation, disposer)
                } else {
                    callback(|_| Ok(JsValue::UNDEFINED))
                })
            }
            "get" => {
                let context = activation.borrow().context.clone();
                bridge::get_key(&context, &args.get(0))
            }
            "timeout" | "setTimeout" | "interval" | "setInterval" => timer(
                &activation,
                &args,
                matches!(name, "interval" | "setInterval"),
            ),
            _ => {
                let event = bridge::string(&args.get(0))?;
                let arguments = args
                    .iter()
                    .skip(1)
                    .map(|value| wire(&value))
                    .collect::<Result<Vec<_>, _>>()?;
                let promise = call(
                    &realm,
                    &activation,
                    json!({"type":"event","mode":name,"name":event,"args":arguments}),
                )?;
                Ok(if name == "emit" {
                    JsValue::UNDEFINED
                } else {
                    promise
                })
            }
        });
        set(&ctx, name, &function)?;
    }
    let reflect = bridge::object(&[("provide", bridge::get(&ctx, "provide")?)])?;
    if request["hasTools"] == true {
        crate::loader_tools::install(realm, activation, &ctx, &request["toolSchemas"])?;
    }
    set(&ctx, "reflect", &reflect)?;
    set(&ctx, "root", &ctx)?;
    if let Some(base) = request["baseUrl"].as_str() {
        set(&ctx, "baseUrl", &JsValue::from_str(base))?;
    }
    attach_fiber(realm, activation, request, &ctx)?;
    attach_logger(realm, activation, &ctx)?;
    Ok(ctx)
}

fn attach_fiber(
    realm: &Rc<RefCell<Realm>>,
    activation: &Rc<RefCell<Activation>>,
    request: &Value,
    ctx: &JsValue,
) -> Result<(), JsValue> {
    let owner = realm.clone();
    let active = activation.clone();
    let dispose = callback(move |_| call(&owner, &active, json!({"type":"disposeRoot"})));
    let config = bridge::from_json(&request["config"])?;
    activation.borrow_mut().config = config.clone();
    let fiber = bridge::object(&[
        ("uid", bridge::from_json(&json!(activation.borrow().id))?),
        ("state", JsValue::from_f64(1.0)),
        ("dispose", dispose),
        ("_config", config.clone()),
        ("config", config),
        ("ctx", ctx.clone()),
        ("parent", ctx.clone()),
        ("entry", bridge::from_json(&request["entry"])?),
    ])?;
    activation.borrow_mut().fiber = fiber.clone();
    set(ctx, "fiber", &fiber)
}

fn attach_logger(
    realm: &Rc<RefCell<Realm>>,
    activation: &Rc<RefCell<Activation>>,
    ctx: &JsValue,
) -> Result<(), JsValue> {
    let logger = bridge::object(&[])?;
    for level in ["debug", "info", "warn", "error", "success"] {
        let owner = realm.clone();
        let active = activation.clone();
        let function = callback(move |args| {
            let arguments = args
                .iter()
                .map(|arg| wire(&arg).unwrap_or_else(|_| json!(bridge::message(&arg))))
                .collect::<Vec<_>>();
            call(
                &owner,
                &active,
                json!({"type":"log","level":level,"args":arguments}),
            )?;
            Ok(JsValue::UNDEFINED)
        });
        set(&logger, level, &function)?;
    }
    set(ctx, "logger", &logger)
}

fn call_apply(apply: &JsValue, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    if bridge::get(apply, "prototype")?.is_truthy() && !is_generator(apply) {
        let instance = bridge::construct(apply, &bridge::array(arguments)?)?;
        let hooks = bridge::get_key(&instance, &symbol("cordis.initHooks"))?;
        if !hooks.is_undefined() && !hooks.is_null() {
            for hook in Array::from(&hooks).iter() {
                bridge::apply(&hook, &JsValue::UNDEFINED, &[])?;
            }
        }
        let init = bridge::get_key(&instance, &symbol("cordis.init"))?;
        if init.is_function() {
            bridge::apply(&init, &instance, &[])
        } else {
            Ok(JsValue::UNDEFINED)
        }
    } else {
        bridge::apply(apply, &JsValue::UNDEFINED, arguments)
    }
}

pub(crate) async fn activate(
    realm: &Rc<RefCell<Realm>>,
    plugin: &JsValue,
    request: &Value,
) -> Result<Value, JsValue> {
    let id = request["activation"]
        .as_u64()
        .ok_or_else(|| bridge::error("missing activation identity"))?;
    let activation = Rc::new(RefCell::new(Activation {
        id,
        module: request["module"].as_u64().unwrap_or_default(),
        native_key: request["nativeFiber"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        fiber: JsValue::UNDEFINED,
        config: JsValue::UNDEFINED,
        context: JsValue::UNDEFINED,
        callbacks: BTreeMap::new(),
        disposers: Vec::new(),
        pending: Vec::new(),
        retained: Vec::new(),
        services: Vec::new(),
        next_effect: 1,
        disposed: false,
        failed: false,
        tools: BTreeMap::new(),
    }));
    realm
        .borrow_mut()
        .activations
        .insert(id, activation.clone());
    let ctx = context(realm, &activation, request)?;
    let apply = if plugin.is_function() {
        plugin.clone()
    } else {
        bridge::get(plugin, "apply")?
    };
    let arguments = [ctx, activation.borrow().config.clone()];
    let result = call_apply(&apply, &arguments);
    let result = match result {
        Ok(value) => settled(value).await,
        Err(error) => Err(error),
    };
    let result = drain(&activation).await.and(result);
    activation.borrow_mut().failed = result.is_err();
    if result.is_ok() {
        set(&activation.borrow().fiber, "state", &JsValue::from_f64(2.0))?;
    }
    result?;
    Ok(Value::Null)
}

pub(crate) async fn deactivate(realm: &Rc<RefCell<Realm>>, id: u64) -> Result<Value, JsValue> {
    let Some(activation) = realm.borrow().activations.get(&id).cloned() else {
        return Ok(Value::Null);
    };
    if activation.borrow().disposed {
        return Ok(Value::Null);
    }
    activation.borrow_mut().disposed = true;
    let fiber = activation.borrow().fiber.clone();
    set(&fiber, "uid", &JsValue::NULL)?;
    set(&fiber, "state", &JsValue::from_f64(5.0))?;
    let disposers = std::mem::take(&mut activation.borrow_mut().disposers);
    let mut pending = Vec::new();
    let mut errors = Vec::new();
    for disposer in disposers.into_iter().rev() {
        match bridge::apply(&disposer, &JsValue::UNDEFINED, &[]) {
            Ok(value) => pending.push(value),
            Err(error) => errors.push(bridge::message(&error)),
        }
    }
    for value in pending {
        if let Err(error) = settled(value).await {
            errors.push(bridge::message(&error));
        }
    }
    if let Err(error) = drain(&activation).await {
        errors.push(bridge::message(&error));
    }
    realm.borrow_mut().activations.remove(&id);
    set(
        &fiber,
        "state",
        &JsValue::from_f64(if activation.borrow().failed { 3.0 } else { 4.0 }),
    )?;
    for service in &activation.borrow().services {
        realm.borrow_mut().services.remove(service);
    }
    activation.borrow_mut().retained.clear();
    activation.borrow_mut().callbacks.clear();
    activation.borrow_mut().tools.clear();
    activation.borrow_mut().context = JsValue::UNDEFINED;
    activation.borrow_mut().fiber = JsValue::UNDEFINED;
    activation.borrow_mut().config = JsValue::UNDEFINED;
    if !errors.is_empty() {
        return Err(bridge::error(&errors.join("; ")));
    }
    Ok(Value::Null)
}

pub(crate) async fn invoke(realm: &Rc<RefCell<Realm>>, request: &Value) -> Result<Value, JsValue> {
    let id = request["activation"].as_u64().unwrap_or_default();
    let activation = realm
        .borrow()
        .activations
        .get(&id)
        .cloned()
        .ok_or_else(|| bridge::error("plugin activation is disposed"))?;
    let (receiver, function) = if let Some(service) = request["service"].as_u64() {
        let receiver = realm
            .borrow()
            .services
            .get(&service)
            .cloned()
            .ok_or_else(|| bridge::error("plugin service is disposed"))?;
        let method = request["method"].as_str().unwrap_or_default();
        let function = bridge::get(&receiver, method)?;
        (receiver, function)
    } else {
        let effect = request["callback"].as_u64().unwrap_or_default();
        let function = activation
            .borrow()
            .callbacks
            .get(&effect)
            .cloned()
            .ok_or_else(|| bridge::error("plugin callback is disposed"))?;
        (activation.borrow().context.clone(), function)
    };
    let args = if request["argumentKinds"] == true {
        let values = request["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|value| match value["kind"].as_str() {
                Some("hmrReload") => Ok(realm.borrow().last_reload.clone()),
                Some("error") => Ok(bridge::error(value["value"].as_str().unwrap_or_default())),
                _ => bridge::from_json(&value["value"]),
            })
            .collect::<Result<Vec<_>, JsValue>>()?;
        bridge::array(&values)?
    } else {
        bridge::from_json(&request["args"])?
    };
    let value = settled(bridge::apply(
        &function,
        &receiver,
        &Array::from(&args).iter().collect::<Vec<_>>(),
    )?)
    .await?;
    drain(&activation).await?;
    wire(&value)
}
