//! JavaScript-bound Remote mount lifecycle.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::INJECT;

thread_local! {
static CONTRIBUTIONS: RefCell<Option<Vec<JsValue>>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy)]
enum SchemaKind {
    Any,
    String,
    Object,
    GoalRef,
    CreateGoalRequest,
    EditGoalRequest,
    CreateGoalResult,
    GoalView,
}

#[derive(Clone, Copy)]
struct ParameterSpec {
    name: &'static str,
    wire: &'static str,
    source: &'static str,
    lookup: Option<&'static str>,
    schema: SchemaKind,
}

/// Returns the five generated Remote contributions as Rust-owned browser data.
///
/// # Errors
///
/// Returns JavaScript object-construction failures.
#[wasm_bindgen(js_name = generatedApiRemotes)]
pub fn generated_api_remotes() -> Result<Array, JsValue> {
    Ok([
        commands_contribution()?,
        goals_contribution()?,
        dynamic_contribution()?,
        inventory_contribution()?,
        feedback_contribution()?,
    ]
    .into_iter()
    .collect())
}

fn commands_contribution() -> Result<JsValue, JsValue> {
    contribution(
        "@seekdeep-ai/seekdeep-commands",
        vec![
            descriptor(
                "commands",
                "execute",
                vec![
                    agent_parameter(),
                    json_parameter("line", SchemaKind::String),
                ],
                true,
                true,
                SchemaKind::Any,
            )?,
            descriptor(
                "commands",
                "list",
                vec![agent_parameter()],
                true,
                false,
                SchemaKind::Any,
            )?,
        ],
    )
}

fn goals_contribution() -> Result<JsValue, JsValue> {
    contribution(
        "@seekdeep-ai/seekdeep-goal",
        vec![
            descriptor(
                "goals",
                "clear",
                vec![
                    agent_parameter(),
                    json_parameter("ref", SchemaKind::GoalRef),
                ],
                true,
                false,
                SchemaKind::GoalRef,
            )?,
            descriptor(
                "goals",
                "complete",
                vec![
                    agent_parameter(),
                    json_parameter("ref", SchemaKind::GoalRef),
                ],
                true,
                false,
                SchemaKind::GoalView,
            )?,
            descriptor(
                "goals",
                "create",
                vec![
                    agent_parameter(),
                    json_parameter("request", SchemaKind::CreateGoalRequest),
                ],
                true,
                false,
                SchemaKind::CreateGoalResult,
            )?,
            descriptor(
                "goals",
                "edit",
                vec![
                    agent_parameter(),
                    json_parameter("ref", SchemaKind::GoalRef),
                    json_parameter("request", SchemaKind::EditGoalRequest),
                ],
                true,
                false,
                SchemaKind::GoalView,
            )?,
            descriptor(
                "goals",
                "pause",
                vec![
                    agent_parameter(),
                    json_parameter("ref", SchemaKind::GoalRef),
                ],
                true,
                false,
                SchemaKind::GoalView,
            )?,
            descriptor(
                "goals",
                "resume",
                vec![
                    agent_parameter(),
                    json_parameter("ref", SchemaKind::GoalRef),
                ],
                true,
                false,
                SchemaKind::GoalView,
            )?,
        ],
    )
}

fn dynamic_contribution() -> Result<JsValue, JsValue> {
    let mut descriptors = Vec::new();
    for (method, names, scoped) in [
        ("undefineFromPanel", &["pluginId"][..], true),
        (
            "runHostHalf",
            &[
                "pluginId",
                "packageId",
                "mode",
                "requestId",
                "approveFutureVersions",
            ][..],
            true,
        ),
        ("getClientCode", &["pluginId", "pluginRunId"][..], true),
        ("resolveRequestRun", &["requestId", "resolution"][..], false),
        ("settleUserRun", &["pluginId", "resolution"][..], true),
        ("stopFromPanel", &["pluginId"][..], true),
        ("syncInspectManifest", &["providers"][..], false),
        (
            "resolveInspectQuery",
            &["requestId", "resolution"][..],
            true,
        ),
        ("inventory", &[][..], false),
        (
            "reportRenderFailure",
            &["pluginId", "pluginRunId", "failure"][..],
            true,
        ),
        (
            "reportClientGuardFailure",
            &["pluginId", "pluginRunId", "failure"][..],
            true,
        ),
        (
            "invoke",
            &["pluginId", "pluginRunId", "method", "args"][..],
            false,
        ),
    ] {
        let mut parameters = Vec::new();
        if scoped {
            parameters.push(agent_parameter());
        }
        parameters.extend(
            names
                .iter()
                .map(|name| json_parameter(name, SchemaKind::Any)),
        );
        descriptors.push(descriptor(
            "dynamicCordisRunner",
            method,
            parameters,
            scoped,
            false,
            SchemaKind::Any,
        )?);
    }
    contribution("@seekdeep-ai/seekdeep-cordis-host-runner", descriptors)
}

fn inventory_contribution() -> Result<JsValue, JsValue> {
    contribution(
        "@seekdeep-ai/seekdeep-host-plugin-inventory",
        vec![descriptor(
            "pluginInventory",
            "list",
            Vec::new(),
            false,
            false,
            SchemaKind::Any,
        )?],
    )
}

fn feedback_contribution() -> Result<JsValue, JsValue> {
    contribution(
        "@seekdeep-ai/seekdeep-message-feedback",
        ["list", "put", "delete"]
            .into_iter()
            .map(|method| {
                descriptor(
                    "messageFeedback",
                    method,
                    vec![json_parameter("request", SchemaKind::Object)],
                    false,
                    false,
                    SchemaKind::Any,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    )
}

fn contribution(package: &str, descriptors: Vec<JsValue>) -> Result<JsValue, JsValue> {
    let descriptors: Array = descriptors.into_iter().collect();
    object(&[
        ("package", JsValue::from_str(package)),
        ("descriptors", descriptors.into()),
    ])
    .map(Into::into)
}

fn agent_parameter() -> ParameterSpec {
    ParameterSpec {
        name: "agent",
        wire: "agentId",
        source: "lookup",
        lookup: Some("agent"),
        schema: SchemaKind::String,
    }
}

fn json_parameter(name: &'static str, schema: SchemaKind) -> ParameterSpec {
    ParameterSpec {
        name,
        wire: name,
        source: "json",
        lookup: None,
        schema,
    }
}

fn descriptor(
    namespace: &str,
    method: &str,
    parameters: Vec<ParameterSpec>,
    scoped: bool,
    cancellation: bool,
    result: SchemaKind,
) -> Result<JsValue, JsValue> {
    let values: Array = parameters
        .into_iter()
        .map(parameter)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .collect();
    let mut fields = vec![
        (
            "id",
            JsValue::from_str(&format!(
                "@seekdeep-ai/seekdeep-api-remotes#{namespace}/{method}"
            )),
        ),
        ("service", JsValue::from_str(namespace)),
        ("namespace", JsValue::from_str(namespace)),
        ("method", JsValue::from_str(method)),
        (
            "invocation",
            object(&[("kind", JsValue::from_str("direct"))])?.into(),
        ),
        ("parameters", values.into()),
        ("result", codec(result)?.into()),
    ];
    if scoped {
        fields.push((
            "scope",
            object(&[
                ("context", JsValue::from_str("agent")),
                ("wire", JsValue::from_str("agentId")),
            ])?
            .into(),
        ));
    }
    if cancellation {
        fields.push(("cancellation", JsValue::TRUE));
    }
    object(&fields).map(Into::into)
}

fn parameter(spec: ParameterSpec) -> Result<JsValue, JsValue> {
    let mut fields = vec![
        ("name", JsValue::from_str(spec.name)),
        ("wire", JsValue::from_str(spec.wire)),
        ("source", JsValue::from_str(spec.source)),
        ("codec", codec(spec.schema)?.into()),
    ];
    if let Some(lookup) = spec.lookup {
        fields.push(("lookup", JsValue::from_str(lookup)));
    }
    object(&fields).map(Into::into)
}

fn codec(kind: SchemaKind) -> Result<Object, JsValue> {
    let parse = Closure::wrap(Box::new(move |value: JsValue| validate(kind, &value))
        as Box<dyn Fn(JsValue) -> Result<JsValue, JsValue>>);
    let schema = object(&[("parse", parse.into_js_value())])?;
    object(&[
        ("mode", JsValue::from_str("strict")),
        ("schema", schema.into()),
    ])
}

fn validate(kind: SchemaKind, value: &JsValue) -> Result<JsValue, JsValue> {
    match kind {
        SchemaKind::Any => Ok(value.clone()),
        SchemaKind::String => value
            .as_string()
            .map(|_| value.clone())
            .ok_or_else(|| js_sys::TypeError::new("expected string").into()),
        SchemaKind::Object => {
            require_object(value)?;
            Ok(value.clone())
        }
        SchemaKind::GoalRef => validate_goal_ref(value),
        SchemaKind::CreateGoalRequest => validate_create_goal_request(value),
        SchemaKind::EditGoalRequest => validate_edit_goal_request(value),
        SchemaKind::CreateGoalResult => {
            let value = require_object(value)?;
            validate_goal_ref(&required(&value, "ref")?)?;
            Ok(value.into())
        }
        SchemaKind::GoalView => validate_goal_view(value),
    }
}

fn validate_goal_ref(value: &JsValue) -> Result<JsValue, JsValue> {
    let value = require_object(value)?;
    require_string(&value, "id")?;
    require_number(&value, "revision")?;
    Ok(value.into())
}

fn validate_create_goal_request(value: &JsValue) -> Result<JsValue, JsValue> {
    let value = require_object(value)?;
    let output = Object::new();
    Reflect::set(
        &output,
        &JsValue::from_str("objective"),
        &JsValue::from_str(&require_string(&value, "objective")?),
    )?;
    if let Some(rounds) = optional_number(&value, "maxGoalRounds")? {
        Reflect::set(
            &output,
            &JsValue::from_str("maxGoalRounds"),
            &JsValue::from_f64(rounds),
        )?;
    }
    Ok(output.into())
}

fn validate_edit_goal_request(value: &JsValue) -> Result<JsValue, JsValue> {
    let value = require_object(value)?;
    let output = Object::new();
    if let Some(objective) = optional_string(&value, "objective")? {
        Reflect::set(
            &output,
            &JsValue::from_str("objective"),
            &JsValue::from_str(&objective),
        )?;
    }
    if let Some(rounds) = optional_number(&value, "maxGoalRounds")? {
        Reflect::set(
            &output,
            &JsValue::from_str("maxGoalRounds"),
            &JsValue::from_f64(rounds),
        )?;
    }
    Ok(output.into())
}

fn validate_goal_view(value: &JsValue) -> Result<JsValue, JsValue> {
    let value = require_object(value)?;
    for key in [
        "roundsStarted",
        "createdAt",
        "updatedAt",
        "maxGoalRounds",
        "revision",
    ] {
        require_number(&value, key)?;
    }
    for key in ["activation", "objective", "phase", "id"] {
        require_string(&value, key)?;
    }
    let blocked = Reflect::get(&value, &JsValue::from_str("blockedReason"))?;
    if !blocked.is_undefined() {
        let blocked = require_object(&blocked)?;
        require_string(&blocked, "code")?;
        require_string(&blocked, "message")?;
    }
    Ok(value.into())
}

fn require_object(value: &JsValue) -> Result<Object, JsValue> {
    if !value.is_object() || value.is_null() || Array::is_array(value) {
        Err(js_sys::TypeError::new("expected object").into())
    } else {
        Ok(Object::from(value.clone()))
    }
}

fn required(value: &Object, key: &str) -> Result<JsValue, JsValue> {
    let field = Reflect::get(value, &JsValue::from_str(key))?;
    if field.is_undefined() {
        Err(js_sys::TypeError::new(&format!("required field {key:?} is missing")).into())
    } else {
        Ok(field)
    }
}

fn require_string(value: &Object, key: &str) -> Result<String, JsValue> {
    required(value, key)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("field {key:?} must be a string")).into())
}

fn require_number(value: &Object, key: &str) -> Result<f64, JsValue> {
    required(value, key)?
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| js_sys::TypeError::new(&format!("field {key:?} must be a number")).into())
}

fn optional_string(value: &Object, key: &str) -> Result<Option<String>, JsValue> {
    let field = Reflect::get(value, &JsValue::from_str(key))?;
    if field.is_undefined() {
        Ok(None)
    } else {
        field.as_string().map(Some).ok_or_else(|| {
            js_sys::TypeError::new(&format!("field {key:?} must be a string")).into()
        })
    }
}

fn optional_number(value: &Object, key: &str) -> Result<Option<f64>, JsValue> {
    let field = Reflect::get(value, &JsValue::from_str(key))?;
    if field.is_undefined() {
        Ok(None)
    } else {
        field
            .as_f64()
            .filter(|number| number.is_finite())
            .map(Some)
            .ok_or_else(|| {
                js_sys::TypeError::new(&format!("field {key:?} must be a number")).into()
            })
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry)?;
    }
    Ok(value)
}

/// Configures the five generated Remote contributions at module materialization.
///
/// # Errors
///
/// Rejects a non-array or wrong-cardinality module factory handoff.
#[wasm_bindgen(js_name = configureApiRemotes)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_api_remotes(contributions: JsValue) -> Result<(), JsValue> {
    if !Array::is_array(&contributions) {
        return Err(js_error(
            "api-remotes: generated contributions must be an array",
        ));
    }
    let contributions = Array::from(&contributions).to_vec();
    if contributions.len() != 5 {
        return Err(js_error(&format!(
            "api-remotes: expected five generated contributions, got {}",
            contributions.len()
        )));
    }
    CONTRIBUTIONS.with(|slot| *slot.borrow_mut() = Some(contributions));
    Ok(())
}

/// Mounts every selected namespace and resolves to its reverse-order disposer.
#[wasm_bindgen(js_name = applyApiRemotes)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_api_remotes(ctx: JsValue) -> Promise {
    future_to_promise(async move {
        let remote = required_service(&ctx, "remote")?;
        let contributions = CONTRIBUTIONS
            .with(|slot| slot.borrow().clone())
            .ok_or_else(|| {
                js_error("api-remotes module factory did not configure generated contributions")
            })?;
        let mut disposers = Vec::new();
        for contribution in contributions {
            match await_method(&remote, "$mount", &[contribution]).await {
                Ok(disposer) => disposers.push(disposer.dyn_into::<Function>()?),
                Err(error) => {
                    dispose_reverse(&mut disposers).await?;
                    return Err(error);
                }
            }
        }
        let disposers = Rc::new(RefCell::new(disposers));
        let disposer = Closure::wrap(Box::new(move || -> Promise {
            let disposers = Rc::clone(&disposers);
            future_to_promise(async move {
                let ordered = {
                    let mut disposers = disposers.borrow_mut();
                    let mut ordered = std::mem::take(&mut *disposers);
                    ordered.reverse();
                    ordered
                };
                dispose_ordered(&ordered).await?;
                Ok(JsValue::UNDEFINED)
            })
        }) as Box<dyn FnMut() -> Promise>);
        Ok(disposer.into_js_value())
    })
}

/// Exact static inject list.
#[wasm_bindgen(js_name = apiRemotesInject)]
pub fn api_remotes_inject() -> Array {
    let values = Array::new();
    for dependency in INJECT {
        values.push(&JsValue::from_str(dependency));
    }
    values
}

async fn dispose_reverse(disposers: &mut [Function]) -> Result<(), JsValue> {
    disposers.reverse();
    dispose_ordered(disposers).await
}

async fn dispose_ordered(disposers: &[Function]) -> Result<(), JsValue> {
    for disposer in disposers {
        let result = disposer.call0(&JsValue::UNDEFINED)?;
        JsFuture::from(Promise::resolve(&result)).await?;
    }
    Ok(())
}

async fn await_method(
    value: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let result = call_method(value, name, arguments)?;
    JsFuture::from(Promise::resolve(&result)).await
}

fn required_service(ctx: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    let service = call_method(ctx, "get", &[JsValue::from_str(name)])?;
    if service.is_undefined() || service.is_null() {
        Err(js_error(&format!(
            "api-remotes requires Client Service {name:?}"
        )))
    } else {
        Ok(service)
    }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}
