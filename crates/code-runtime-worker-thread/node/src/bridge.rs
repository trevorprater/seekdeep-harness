//! Captured JavaScript intrinsics and foreign-call adapters.

use js_sys::{Array, Function};
use wasm_bindgen::prelude::*;

use crate::CodeJsonString;

#[wasm_bindgen(inline_js = r"
const apply = Reflect.apply;
const construct = Reflect.construct;
const get = Reflect.get;
const set = Reflect.set;
const ownKeys = Reflect.ownKeys;
const create = Object.create;
const define = Object.defineProperty;
const descriptor = Object.getOwnPropertyDescriptor;
const prototype = Object.getPrototypeOf;
const hasOwn = Object.hasOwn;
const isArray = Array.isArray;
const stringify = JSON.stringify;
const parse = JSON.parse;
const string = String;
const functionSource = Function.prototype.toString;
const error = Error;
const setConstructor = Set;
const setHas = Set.prototype.has;
const setAdd = Set.prototype.add;
const setDelete = Set.prototype.delete;
const promiseThen = Promise.prototype.then;
const promiseResolve = Promise.resolve;
const promiseReject = Promise.reject;
const promiseConstructor = Promise;
const asyncConstructor = prototype(async function() {}).constructor;
const functionConstructor = Function;
export function intrinsicApply(fn, receiver, args) { return apply(fn, receiver, args); }
export function intrinsicConstruct(fn, args) { return construct(fn, args); }
export function intrinsicGet(value, key) { return get(value, key); }
export function intrinsicSet(value, key, item) { return set(value, key, item); }
export function intrinsicOwnKeys(value) { return ownKeys(value); }
export function intrinsicCreate(proto) { return create(proto); }
export function intrinsicDefine(value, key, attributes) { return define(value, key, attributes); }
export function intrinsicDescriptor(value, key) { return descriptor(value, key); }
export function intrinsicPrototype(value) { return prototype(value); }
export function intrinsicHasOwn(value, key) { return hasOwn(value, key); }
export function intrinsicIsArray(value) { return isArray(value); }
export function intrinsicString(value) { return string(value); }
export function intrinsicParse(value) { return parse(value); }
export function intrinsicStringify(value) { return stringify(value); }
export function intrinsicFunctionSource(value) { return apply(functionSource, value, []); }
export function intrinsicIsError(value) { return value instanceof error; }
export function intrinsicError(message) { return new error(message); }
export function intrinsicNewSet() { return new setConstructor(); }
export function intrinsicSetHas(value, item) { return apply(setHas, value, [item]); }
export function intrinsicSetAdd(value, item) { return apply(setAdd, value, [item]); }
export function intrinsicSetDelete(value, item) { return apply(setDelete, value, [item]); }
export function intrinsicThen(value, resolve, reject) { return apply(promiseThen, value, [resolve, reject]); }
export function intrinsicResolve(value) { return apply(promiseResolve, promiseConstructor, [value]); }
export function intrinsicReject(value) { return apply(promiseReject, promiseConstructor, [value]); }
export function intrinsicPromise(executor) { return new promiseConstructor(executor); }
export function intrinsicAsyncFunction(args) { return construct(asyncConstructor, args); }
export function intrinsicFunction(args) { return construct(functionConstructor, args); }
export function variadic(callback) { return (...args) => callback(args); }
export function unary(callback) { return {value: argument => callback(argument)}.value; }
export function newArray() { return []; }
")]
extern "C" {
    #[wasm_bindgen(catch, js_name = intrinsicApply)]
    fn apply_raw(
        function: &JsValue,
        receiver: &JsValue,
        args: &JsValue,
    ) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicConstruct)]
    pub(crate) fn construct(function: &JsValue, args: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicGet)]
    pub(crate) fn get_key(value: &JsValue, key: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicSet)]
    pub(crate) fn set_key(value: &JsValue, key: &JsValue, item: &JsValue) -> Result<bool, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicOwnKeys)]
    pub(crate) fn own_keys(value: &JsValue) -> Result<Array, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicCreate)]
    pub(crate) fn create(prototype: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicDefine)]
    fn define_raw(value: &JsValue, key: &JsValue, attributes: &JsValue)
    -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicDescriptor)]
    pub(crate) fn descriptor(value: &JsValue, key: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicPrototype)]
    pub(crate) fn prototype(value: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicHasOwn)]
    pub(crate) fn has_own(value: &JsValue, key: &JsValue) -> Result<bool, JsValue>;
    #[wasm_bindgen(js_name = intrinsicIsArray)]
    pub(crate) fn is_array(value: &JsValue) -> bool;
    #[wasm_bindgen(catch, js_name = intrinsicString)]
    pub(crate) fn string(value: &JsValue) -> Result<String, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicString)]
    pub(crate) fn string_value(value: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicParse)]
    pub(crate) fn parse(value: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicStringify)]
    pub(crate) fn stringify(value: &JsValue) -> Result<String, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicFunctionSource)]
    pub(crate) fn function_source(value: &JsValue) -> Result<String, JsValue>;
    #[wasm_bindgen(js_name = intrinsicIsError)]
    pub(crate) fn is_error(value: &JsValue) -> bool;
    #[wasm_bindgen(js_name = intrinsicError)]
    pub(crate) fn error(message: &str) -> JsValue;
    #[wasm_bindgen(js_name = intrinsicError)]
    pub(crate) fn error_value(message: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = intrinsicNewSet)]
    pub(crate) fn new_set() -> JsValue;
    #[wasm_bindgen(js_name = intrinsicSetHas)]
    pub(crate) fn set_has(value: &JsValue, item: &JsValue) -> bool;
    #[wasm_bindgen(js_name = intrinsicSetAdd)]
    pub(crate) fn set_add(value: &JsValue, item: &JsValue);
    #[wasm_bindgen(js_name = intrinsicSetDelete)]
    pub(crate) fn set_delete(value: &JsValue, item: &JsValue);
    #[wasm_bindgen(catch, js_name = intrinsicThen)]
    pub(crate) fn then(
        value: &JsValue,
        resolve: &JsValue,
        reject: &JsValue,
    ) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = intrinsicResolve)]
    pub(crate) fn resolve(value: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = intrinsicReject)]
    pub(crate) fn reject(value: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = intrinsicPromise)]
    pub(crate) fn promise(executor: &JsValue) -> JsValue;
    #[wasm_bindgen(catch, js_name = intrinsicAsyncFunction)]
    pub(crate) fn async_function(arguments: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = intrinsicFunction)]
    pub(crate) fn function(arguments: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = variadic)]
    pub(crate) fn variadic(callback: &JsValue) -> Function;
    #[wasm_bindgen(js_name = unary)]
    pub(crate) fn unary(callback: &JsValue) -> Function;
    #[wasm_bindgen(js_name = newArray)]
    fn new_array() -> Array;
}

thread_local! {
    static APIS: std::cell::RefCell<Option<JsValue>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn install(apis: JsValue) -> Result<(), JsValue> {
    APIS.with(|slot| {
        if slot.borrow().is_some() {
            return Err(error("Node boundary already started"));
        }
        *slot.borrow_mut() = Some(apis);
        Ok(())
    })
}

pub(crate) fn api(name: &str) -> Result<JsValue, JsValue> {
    APIS.with(|slot| get(slot.borrow().as_ref().expect("installed Node API"), name))
}

pub(crate) fn get(value: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    get_key(value, &JsValue::from_str(key))
}

pub(crate) fn apply(
    function: &JsValue,
    receiver: &JsValue,
    values: &[JsValue],
) -> Result<JsValue, JsValue> {
    apply_raw(function, receiver, &array(values)?)
}

pub(crate) fn method(
    receiver: &JsValue,
    name: &str,
    values: &[JsValue],
) -> Result<JsValue, JsValue> {
    apply(&get(receiver, name)?, receiver, values)
}

pub(crate) fn array(values: &[JsValue]) -> Result<JsValue, JsValue> {
    let array = new_array();
    for (index, value) in values.iter().enumerate() {
        define(
            &array,
            &JsValue::from_str(&index.to_string()),
            value,
            true,
            true,
        )?;
    }
    Ok(array.into())
}

pub(crate) fn object(fields: &[(&str, JsValue)]) -> Result<JsValue, JsValue> {
    let object = create(&JsValue::NULL)?;
    for (key, value) in fields {
        define(&object, &JsValue::from_str(key), value, true, true)?;
    }
    Ok(object)
}

pub(crate) fn define(
    value: &JsValue,
    key: &JsValue,
    item: &JsValue,
    enumerable: bool,
    writable: bool,
) -> Result<(), JsValue> {
    let attributes = create(&JsValue::NULL)?;
    set_key(&attributes, &JsValue::from_str("value"), item)?;
    set_key(
        &attributes,
        &JsValue::from_str("enumerable"),
        &JsValue::from_bool(enumerable),
    )?;
    set_key(
        &attributes,
        &JsValue::from_str("writable"),
        &JsValue::from_bool(writable),
    )?;
    set_key(
        &attributes,
        &JsValue::from_str("configurable"),
        &JsValue::from_bool(writable),
    )?;
    define_raw(value, key, &attributes)?;
    Ok(())
}

pub(crate) fn from_json(value: &serde_json::Value) -> Result<JsValue, JsValue> {
    parse(&serde_json::to_string(value).map_err(|failure| error(&failure.to_string()))?)
}

pub(crate) fn message(value: &JsValue) -> String {
    if is_error(value) {
        get(value, "message").and_then(|message| string(&message))
    } else {
        string(value)
    }
    .unwrap_or_else(|_| "program threw an unrenderable value".to_owned())
}

pub(crate) fn text(value: &JsValue) -> Result<CodeJsonString, JsValue> {
    let value = string_value(value)?;
    CodeJsonString::parse(stringify(&value)?).map_err(|failure| error(&failure.to_string()))
}

pub(crate) fn message_text(value: &JsValue) -> CodeJsonString {
    if is_error(value) {
        get(value, "message").and_then(|value| text(&value))
    } else {
        text(value)
    }
    .unwrap_or_else(|_| "program threw an unrenderable value".into())
}
