//! Typed access to compiler objects hosted in the embedded engine.
//!
//! Every helper reports engine exceptions as analysis errors carrying the
//! JavaScript message, so compiler failures keep their source diagnostics.

use serde_json::Value;

use crate::{Result, TypertGeneratorError};

/// Handle scope shared by every native analysis routine.
pub(crate) type Scope<'s, 'i> = v8::PinScope<'s, 'i>;

/// Engine value handle.
pub(crate) type Val<'s> = v8::Local<'s, v8::Value>;

pub(crate) fn failure(message: impl Into<String>) -> TypertGeneratorError {
    TypertGeneratorError::Analysis(message.into())
}

/// Renders a caught exception the way the source prints thrown errors.
pub(crate) fn exception_message<'s>(scope: &mut Scope<'s, '_>, exception: Val<'s>) -> String {
    if let Some(object) = exception.to_object(scope)
        && let Some(key) = v8::String::new(scope, "message")
        && let Some(message) = object.get(scope, key.into())
        && message.is_string()
    {
        return message.to_rust_string_lossy(scope);
    }
    exception.to_rust_string_lossy(scope)
}

pub(crate) fn string<'s>(scope: &mut Scope<'s, '_>, value: &str) -> Val<'s> {
    v8::String::new(scope, value).map_or_else(|| v8::undefined(scope).into(), Into::into)
}

pub(crate) fn number<'s>(scope: &mut Scope<'s, '_>, value: f64) -> Val<'s> {
    v8::Number::new(scope, value).into()
}

pub(crate) fn boolean<'s>(scope: &mut Scope<'s, '_>, value: bool) -> Val<'s> {
    v8::Boolean::new(scope, value).into()
}

pub(crate) fn undefined<'s>(scope: &mut Scope<'s, '_>) -> Val<'s> {
    v8::undefined(scope).into()
}

pub(crate) fn array<'s>(scope: &mut Scope<'s, '_>, items: &[Val<'s>]) -> Val<'s> {
    v8::Array::new_with_elements(scope, items).into()
}

pub(crate) fn object<'s>(
    scope: &mut Scope<'s, '_>,
    entries: &[(&str, Val<'s>)],
) -> Result<Val<'s>> {
    let object = v8::Object::new(scope);
    for (name, value) in entries {
        set(scope, object.into(), name, *value)?;
    }
    Ok(object.into())
}

pub(crate) fn set<'s>(
    scope: &mut Scope<'s, '_>,
    target: Val<'s>,
    name: &str,
    value: Val<'s>,
) -> Result<()> {
    let object = target
        .to_object(scope)
        .ok_or_else(|| failure(format!("cannot assign {name} on a non-object")))?;
    let key = string(scope, name);
    object
        .set(scope, key, value)
        .ok_or_else(|| failure(format!("cannot assign {name}")))?;
    Ok(())
}

/// Reads one property; absent properties read as `undefined`.
pub(crate) fn get<'s>(scope: &mut Scope<'s, '_>, target: Val<'s>, name: &str) -> Result<Val<'s>> {
    if target.is_null_or_undefined() {
        return Err(failure(format!(
            "cannot read compiler property {name} of {}",
            target.to_rust_string_lossy(scope)
        )));
    }
    let object = target
        .to_object(scope)
        .ok_or_else(|| failure(format!("cannot read compiler property {name}")))?;
    let key = string(scope, name);
    object
        .get(scope, key)
        .ok_or_else(|| failure(format!("cannot read compiler property {name}")))
}

/// Reads a property path such as `factory.updateClassDeclaration`.
pub(crate) fn get_path<'s>(
    scope: &mut Scope<'s, '_>,
    target: Val<'s>,
    path: &str,
) -> Result<Val<'s>> {
    let mut current = target;
    for segment in path.split('.') {
        current = get(scope, current, segment)?;
    }
    Ok(current)
}

pub(crate) fn get_string<'s>(
    scope: &mut Scope<'s, '_>,
    target: Val<'s>,
    name: &str,
) -> Result<String> {
    let value = get(scope, target, name)?;
    Ok(value.to_rust_string_lossy(scope))
}

/// Reads an optional string property; `undefined` and `null` read as absent.
pub(crate) fn get_optional_string<'s>(
    scope: &mut Scope<'s, '_>,
    target: Val<'s>,
    name: &str,
) -> Result<Option<String>> {
    let value = get(scope, target, name)?;
    Ok(if value.is_null_or_undefined() {
        None
    } else {
        Some(value.to_rust_string_lossy(scope))
    })
}

/// Converts a compiler integer (an enumeration, flag, kind, offset, or count).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "compiler integers are small non-negative values"
)]
pub(crate) fn integer(value: f64) -> u32 {
    value as u32
}

/// Converts a compiler text offset or index to a native size.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "compiler offsets are non-negative and bounded by source length"
)]
pub(crate) fn offset(value: f64) -> usize {
    value as usize
}

pub(crate) fn get_number<'s>(
    scope: &mut Scope<'s, '_>,
    target: Val<'s>,
    name: &str,
) -> Result<f64> {
    let value = get(scope, target, name)?;
    value
        .number_value(scope)
        .ok_or_else(|| failure(format!("compiler property {name} is not numeric")))
}

pub(crate) fn get_flags<'s>(scope: &mut Scope<'s, '_>, target: Val<'s>, name: &str) -> Result<u32> {
    let value = get(scope, target, name)?;
    if value.is_undefined() {
        return Ok(0);
    }
    Ok(integer(value.number_value(scope).ok_or_else(|| {
        failure(format!("compiler property {name} is not numeric"))
    })?))
}

pub(crate) fn get_bool<'s>(scope: &mut Scope<'s, '_>, target: Val<'s>, name: &str) -> Result<bool> {
    let value = get(scope, target, name)?;
    Ok(value.boolean_value(scope))
}

pub(crate) fn get_defined<'s>(
    scope: &mut Scope<'s, '_>,
    target: Val<'s>,
    name: &str,
) -> Result<Option<Val<'s>>> {
    let value = get(scope, target, name)?;
    Ok(if value.is_undefined() {
        None
    } else {
        Some(value)
    })
}

/// Reads an optional array property as a vector; absent arrays are empty.
pub(crate) fn get_items<'s>(
    scope: &mut Scope<'s, '_>,
    target: Val<'s>,
    name: &str,
) -> Result<Vec<Val<'s>>> {
    let value = get(scope, target, name)?;
    if value.is_null_or_undefined() {
        return Ok(Vec::new());
    }
    items(scope, value)
}

pub(crate) fn items<'s>(scope: &mut Scope<'s, '_>, value: Val<'s>) -> Result<Vec<Val<'s>>> {
    let array = v8::Local::<v8::Array>::try_from(value)
        .map_err(|_| failure("compiler value is not an array"))?;
    let mut result = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        result.push(
            array
                .get_index(scope, index)
                .ok_or_else(|| failure(format!("cannot read array index {index}")))?,
        );
    }
    Ok(result)
}

pub(crate) fn text<'s>(scope: &mut Scope<'s, '_>, value: Val<'s>) -> String {
    value.to_rust_string_lossy(scope)
}

/// Invokes a method, translating thrown exceptions into analysis errors.
pub(crate) fn call<'s>(
    scope: &mut Scope<'s, '_>,
    receiver: Val<'s>,
    name: &str,
    arguments: &[Val<'s>],
) -> Result<Val<'s>> {
    let function = get(scope, receiver, name)?;
    invoke(scope, function, receiver, arguments)
        .map_err(|error| TypertGeneratorError::Analysis(format!("{name}: {error}")))
}

/// Invokes a function value with an explicit receiver.
pub(crate) fn invoke<'s>(
    scope: &mut Scope<'s, '_>,
    function: Val<'s>,
    receiver: Val<'s>,
    arguments: &[Val<'s>],
) -> Result<Val<'s>> {
    let function = v8::Local::<v8::Function>::try_from(function)
        .map_err(|_| failure("compiler member is not callable"))?;
    v8::tc_scope!(let caught, scope);
    function.call(caught, receiver, arguments).ok_or_else(|| {
        caught.exception().map_or_else(
            || failure("compiler call terminated"),
            |exception| failure(exception_message(caught, exception)),
        )
    })
}

pub(crate) fn evaluate<'s>(scope: &mut Scope<'s, '_>, source: &str, name: &str) -> Result<Val<'s>> {
    v8::tc_scope!(let caught, scope);
    let source =
        v8::String::new(caught, source).ok_or_else(|| failure("compiler source allocation"))?;
    let origin_name =
        v8::String::new(caught, name).ok_or_else(|| failure("compiler origin allocation"))?;
    let origin = v8::ScriptOrigin::new(
        caught,
        origin_name.into(),
        0,
        0,
        false,
        0,
        None,
        false,
        false,
        false,
        None,
    );
    v8::Script::compile(caught, source, Some(&origin))
        .and_then(|script| script.run(caught))
        .ok_or_else(|| {
            caught.exception().map_or_else(
                || failure("compiler terminated"),
                |exception| failure(exception_message(caught, exception)),
            )
        })
}

pub(crate) fn from_json<'s>(scope: &mut Scope<'s, '_>, value: &Value) -> Result<Val<'s>> {
    let encoded = serde_json::to_string(value)
        .map_err(|error| TypertGeneratorError::Model(error.to_string()))?;
    let encoded = v8::String::new(scope, &encoded).ok_or_else(|| failure("JSON allocation"))?;
    v8::json::parse(scope, encoded).ok_or_else(|| failure("cannot decode JSON into the compiler"))
}

pub(crate) fn to_json<'s>(scope: &mut Scope<'s, '_>, value: Val<'s>) -> Result<Value> {
    if value.is_undefined() {
        return Ok(Value::Null);
    }
    let encoded = v8::json::stringify(scope, value)
        .ok_or_else(|| failure("cannot encode compiler value as JSON"))?
        .to_rust_string_lossy(scope);
    serde_json::from_str(&encoded).map_err(|error| TypertGeneratorError::Model(error.to_string()))
}

pub(crate) fn same(left: Val<'_>, right: Val<'_>) -> bool {
    left.strict_equals(right)
}

/// Identity-keyed map over engine values.
#[derive(Clone, Copy)]
pub(crate) struct JsMap<'s>(v8::Local<'s, v8::Map>);

impl<'s> JsMap<'s> {
    pub(crate) fn new(scope: &mut Scope<'s, '_>) -> Self {
        Self(v8::Map::new(scope))
    }

    pub(crate) fn get(self, scope: &mut Scope<'s, '_>, key: Val<'s>) -> Option<Val<'s>> {
        self.0.get(scope, key).filter(|value| !value.is_undefined())
    }

    pub(crate) fn set(self, scope: &mut Scope<'s, '_>, key: Val<'s>, value: Val<'s>) {
        self.0.set(scope, key, value);
    }

    pub(crate) fn has(self, scope: &mut Scope<'s, '_>, key: Val<'s>) -> bool {
        self.0.has(scope, key).unwrap_or(false)
    }

    pub(crate) fn delete(self, scope: &mut Scope<'s, '_>, key: Val<'s>) {
        self.0.delete(scope, key);
    }

    pub(crate) fn value(self) -> Val<'s> {
        self.0.into()
    }

    pub(crate) fn from_value(value: Val<'s>) -> Result<Self> {
        v8::Local::<v8::Map>::try_from(value)
            .map(Self)
            .map_err(|_| failure("compiler value is not a Map"))
    }

    /// Keys and values interleaved in insertion order.
    pub(crate) fn entries(self, scope: &mut Scope<'s, '_>) -> Result<Vec<(Val<'s>, Val<'s>)>> {
        let flat = items(scope, self.0.as_array(scope).into())?;
        Ok(flat
            .chunks(2)
            .filter_map(|pair| match pair {
                [key, value] => Some((*key, *value)),
                _ => None,
            })
            .collect())
    }
}

/// Identity set over engine values.
#[derive(Clone, Copy)]
pub(crate) struct JsSet<'s>(v8::Local<'s, v8::Set>);

impl<'s> JsSet<'s> {
    pub(crate) fn new(scope: &mut Scope<'s, '_>) -> Self {
        Self(v8::Set::new(scope))
    }

    pub(crate) fn add(self, scope: &mut Scope<'s, '_>, key: Val<'s>) {
        self.0.add(scope, key);
    }

    pub(crate) fn has(self, scope: &mut Scope<'s, '_>, key: Val<'s>) -> bool {
        self.0.has(scope, key).unwrap_or(false)
    }

    pub(crate) fn delete(self, scope: &mut Scope<'s, '_>, key: Val<'s>) {
        self.0.delete(scope, key);
    }
}
