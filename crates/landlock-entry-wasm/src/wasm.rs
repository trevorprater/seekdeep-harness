use std::cell::RefCell;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, prelude::*};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = Object)]
    fn boxed(value: &JsValue) -> Object;
}

thread_local! {
    static BINDINGS: RefCell<JsValue> = const { RefCell::new(JsValue::UNDEFINED) };
}

fn get(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    if value.is_null() || value.is_undefined() {
        let target = if value.is_null() { "null" } else { "undefined" };
        return Err(js_sys::TypeError::new(&format!(
            "Cannot read properties of {target} (reading '{key}')"
        ))
        .into());
    }
    Reflect::get(&boxed(value), &JsValue::from_str(key))
}

fn call(value: &JsValue, key: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    get(value, key)?
        .dyn_into::<Function>()?
        .apply(value, &args.iter().cloned().collect::<Array>())
}

fn bindings() -> JsValue {
    BINDINGS.with(|bindings| bindings.borrow().clone())
}

/// Bind the Node process, resolver, URL/path, and child-process boundaries.
#[wasm_bindgen(js_name = configureBindings)]
pub fn configure_bindings(value: JsValue) {
    BINDINGS.with(|bindings| bindings.replace(value));
}

/// The launcher filename shared with the Rust executable.
#[wasm_bindgen(js_name = launcherBin)]
pub fn launcher_bin() -> String {
    seekdeep_landlock_run::LAUNCHER_BIN.to_owned()
}

/// Launcher-level failure status shared with the Rust executable.
#[wasm_bindgen(js_name = launcherFailureExit)]
pub fn launcher_failure_exit() -> i32 {
    seekdeep_landlock_run::LAUNCHER_FAILURE_EXIT
}

/// Resolve the platform package, falling back inside the entry package boundary.
///
/// # Errors
///
/// Returns invalid binding or fallback URL/path errors.
#[wasm_bindgen(js_name = launcherPath)]
pub fn launcher_path(resolve_package_json: JsValue) -> Result<JsValue, JsValue> {
    let bindings = bindings();
    let process = get(&bindings, "process")?;
    let platform = get(&process, "platform")?.as_string().unwrap_or_default();
    let arch = get(&process, "arch")?.as_string().unwrap_or_default();
    let package = format!("@seekdeep-ai/node-addon-landlock-run-{platform}-{arch}");
    let resolver = if resolve_package_json.is_undefined() {
        get(&bindings, "resolve")?
    } else {
        resolve_package_json
    };
    let resolution = (|| {
        let manifest = resolver.dyn_into::<Function>()?.call1(
            &JsValue::UNDEFINED,
            &JsValue::from_str(&format!("{package}/package.json")),
        )?;
        let directory = call(&bindings, "dirname", &[manifest])?;
        call(
            &bindings,
            "join",
            &[
                directory,
                JsValue::from_str("bin"),
                JsValue::from_str(seekdeep_landlock_run::LAUNCHER_BIN),
            ],
        )
    })();
    if let Ok(path) = resolution {
        return Ok(path);
    }
    let url = Reflect::construct(
        &get(&bindings, "URL")?.dyn_into::<Function>()?,
        &Array::of2(
            &JsValue::from_str(&format!(
                "../node_modules/{package}/bin/{}",
                seekdeep_landlock_run::LAUNCHER_BIN
            )),
            &get(&bindings, "moduleUrl")?,
        ),
    )?;
    call(&bindings, "fileURLToPath", &[url])
}

fn append_grants(
    output: &Array,
    grants: &JsValue,
    key: &str,
    flag: &'static str,
) -> Result<(), JsValue> {
    let roots = get(grants, key)?;
    let roots = if roots.is_null() || roots.is_undefined() {
        Array::new().into()
    } else {
        roots
    };
    let callback = Closure::wrap(Box::new(move |root: JsValue| -> JsValue {
        Array::of2(&JsValue::from_str(flag), &root).into()
    }) as Box<dyn FnMut(JsValue) -> JsValue>)
    .into_js_value();
    let flattened = get(&roots, "flatMap")?
        .dyn_into::<Function>()
        .map_err(|_| {
            js_sys::TypeError::new(&format!(
                "(grants.{key} ?? []).flatMap is not a function or its return value is not iterable"
            ))
        })?
        .call1(&roots, &callback)?;
    let Some(iterator) = js_sys::try_iter(&flattened)? else {
        return Err(js_sys::TypeError::new(&format!(
            "(grants.{key} ?? []).flatMap is not a function or its return value is not iterable"
        ))
        .into());
    };
    for root in iterator {
        output.push(&root?);
    }
    Ok(())
}

/// Return read-only grants before read-write grants, preserving caller order.
///
/// # Errors
///
/// Preserves property-access, `flatMap`, and iterator failures from caller values.
#[wasm_bindgen(js_name = grantArgs)]
pub fn grant_args(grants: &JsValue) -> Result<Array, JsValue> {
    let output = Array::new();
    append_grants(&output, grants, "readOnly", "--ro")?;
    append_grants(&output, grants, "readWrite", "--rw")?;
    Ok(output)
}

/// Probe the launcher synchronously with a bounded child process.
///
/// # Errors
///
/// Preserves invalid spawn arguments and option property failures from Node.
#[wasm_bindgen]
pub fn probe(launcher: JsValue, options: JsValue) -> Result<String, JsValue> {
    let bindings = bindings();
    let launcher = if launcher.is_undefined() {
        launcher_path(JsValue::UNDEFINED)?
    } else {
        launcher
    };
    let options = if options.is_undefined() {
        Object::new().into()
    } else {
        options
    };
    let timeout = get(&options, "timeoutMs")?;
    let timeout = if timeout.is_null() || timeout.is_undefined() {
        JsValue::from_f64(2000.0)
    } else {
        timeout
    };
    let spawn_options = Object::new();
    Reflect::set(&spawn_options, &JsValue::from_str("timeout"), &timeout)?;
    Reflect::set(
        &spawn_options,
        &JsValue::from_str("encoding"),
        &JsValue::from_str("utf8"),
    )?;
    Reflect::set(
        &spawn_options,
        &JsValue::from_str("stdio"),
        &Array::of3(
            &JsValue::from_str("ignore"),
            &JsValue::from_str("pipe"),
            &JsValue::from_str("ignore"),
        ),
    )?;
    let result = call(
        &bindings,
        "spawnSync",
        &[
            launcher,
            Array::of1(&JsValue::from_str("--probe")).into(),
            spawn_options.into(),
        ],
    )?;
    if get(&result, "status")?.as_f64() != Some(0.0) {
        return Ok("unusable".to_owned());
    }
    let report = get(&result, "stdout")?.as_string().unwrap_or_default();
    Ok(if report.contains("partially enforced") {
        "partial"
    } else {
        "full"
    }
    .to_owned())
}
