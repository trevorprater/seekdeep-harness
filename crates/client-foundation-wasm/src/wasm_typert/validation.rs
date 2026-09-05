use std::collections::HashSet;

use wasm_bindgen::JsValue;

use super::{error, get, string, values};

pub(super) fn invocation(value: &JsValue) -> Result<(), JsValue> {
    let id = string(value, "id")?;
    nonempty("invocation id", &id)?;
    segment("invocation service key", &string(value, "service")?)?;
    wire("invocation namespace", &string(value, "namespace")?)?;
    wire("invocation method", &string(value, "method")?)?;
    if !get(value, "implementation")?.is_undefined() {
        wire(
            "invocation implementation method",
            &string(value, "implementation")?,
        )?;
    }
    codec(&get(value, "result")?, &format!("{id} result"))?;
    let parameters = values(&get(value, "parameters")?)?;
    let mut wires = HashSet::new();
    for parameter in &parameters {
        let name = string(parameter, "name")?;
        let field = string(parameter, "wire")?;
        wire("parameter name", &name)?;
        wire("parameter wire field", &field)?;
        if !wires.insert(field.clone()) {
            return Err(error(&format!(
                "typert: invocation \"{id}\" repeats wire field \"{field}\""
            )));
        }
        if get(parameter, "source")? == "lookup" {
            if !get(parameter, "acceptsUndefined")?.is_undefined() {
                return Err(error(&format!(
                    "typert: invocation \"{id}\" lookup parameter \"{name}\" cannot accept undefined"
                )));
            }
            if get(parameter, "lookup")?.is_undefined() {
                return Err(error(&format!(
                    "typert: invocation \"{id}\" lookup parameter \"{name}\" has no lookup key"
                )));
            }
            segment("lookup key", &string(parameter, "lookup")?)?;
        } else if !get(parameter, "lookup")?.is_undefined() {
            return Err(error(&format!(
                "typert: invocation \"{id}\" JSON parameter \"{name}\" declares a lookup key"
            )));
        }
        codec(&get(parameter, "codec")?, &format!("{id} parameter {name}"))?;
    }
    let cancellation = get(value, "cancellation")?;
    if !cancellation.is_undefined() && get(&cancellation, "parameter")? != "signal" {
        return Err(error(&format!(
            "typert: invocation \"{id}\" cancellation parameter must be \"signal\""
        )));
    }
    let receiver = get(value, "invocation")?;
    let scope = get(value, "scope")?;
    if !scope.is_undefined() {
        validate_scope(&scope, &receiver, &parameters, &id)?;
    }
    if get(&receiver, "kind")? == "context" {
        segment("Context key", &string(&receiver, "context")?)?;
        let field = string(&receiver, "wire")?;
        wire("Context wire field", &field)?;
        if wires.contains(&field) {
            return Err(error(&format!(
                "typert: invocation \"{id}\" repeats wire field \"{field}\""
            )));
        }
        codec(&get(&receiver, "codec")?, &format!("{id} Context"))?;
    }
    Ok(())
}

fn validate_scope(
    scope: &JsValue,
    receiver: &JsValue,
    parameters: &[JsValue],
    id: &str,
) -> Result<(), JsValue> {
    if get(receiver, "kind")? != "direct" {
        return Err(error(&format!(
            "typert: invocation \"{id}\" Context receiver cannot declare a direct scope projection"
        )));
    }
    let context = string(scope, "context")?;
    let field = string(scope, "wire")?;
    segment("scope Context key", &context)?;
    wire("scope wire field", &field)?;
    let mut lookups = Vec::new();
    for parameter in parameters {
        if get(parameter, "source")? == "lookup" {
            lookups.push(parameter);
        }
    }
    if lookups.len() != 1
        || get(lookups[0], "wire")? != field
        || get(lookups[0], "lookup")? != context
    {
        return Err(error(&format!(
            "typert: invocation \"{id}\" scope wire \"{field}\" must select its only lookup parameter"
        )));
    }
    Ok(())
}

fn codec(value: &JsValue, subject: &str) -> Result<(), JsValue> {
    if get(value, "mode")? == "src-json" {
        return Ok(());
    }
    nonempty(
        &format!("{subject} type symbol"),
        &string(value, "typeSymbol")?,
    )?;
    if !get(&get(value, "schema")?, "parse")?.is_function() {
        return Err(error(&format!(
            "typert: {subject} strict codec has no parse() method"
        )));
    }
    Ok(())
}

pub(super) fn wire(subject: &str, value: &str) -> Result<(), JsValue> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || b"_$.-".contains(&value))
    {
        return Err(error(&format!(
            "typert: invalid {subject} \"{value}\" — must contain only RPC endpoint segment characters"
        )));
    }
    Ok(())
}

pub(super) fn segment(subject: &str, value: &str) -> Result<(), JsValue> {
    if value.is_empty() || value.contains('#') {
        return Err(error(&format!(
            "typert: invalid {subject} \"{value}\" — must be nonempty and must not contain \"#\""
        )));
    }
    Ok(())
}

pub(super) fn nonempty(subject: &str, value: &str) -> Result<(), JsValue> {
    if value.is_empty() {
        return Err(error(&format!(
            "typert: invalid {subject} — must be nonempty"
        )));
    }
    Ok(())
}
