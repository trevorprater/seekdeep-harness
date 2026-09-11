//! Lossless JSON construction for trajectory state and projection values.

use std::sync::LazyLock;

use seekdeep_lossless_json::JsonValue;

static NULL: LazyLock<JsonValue> = LazyLock::new(|| serde_json::Value::Null.into());

pub(crate) fn null() -> &'static JsonValue {
    &NULL
}

pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: JsonValue) -> serde_json::Result<T> {
    value.deserialize()
}

macro_rules! json {
    (@array [$($values:expr,)*]) => {
        seekdeep_lossless_json::JsonValue::array(&[$($values,)*])
    };
    (@array [$($values:expr,)*] null, $($rest:tt)*) => {
        $crate::json_value::json!(@array [$($values,)* $crate::json_value::json!(null),] $($rest)*)
    };
    (@array [$($values:expr,)*] [$($value:tt)*], $($rest:tt)*) => {
        $crate::json_value::json!(@array [$($values,)* $crate::json_value::json!([$($value)*]),] $($rest)*)
    };
    (@array [$($values:expr,)*] {$($value:tt)*}, $($rest:tt)*) => {
        $crate::json_value::json!(@array [$($values,)* $crate::json_value::json!({$($value)*}),] $($rest)*)
    };
    (@array [$($values:expr,)*] $value:expr, $($rest:tt)*) => {
        $crate::json_value::json!(@array [$($values,)* $crate::json_value::json!($value),] $($rest)*)
    };
    (@array [$($values:expr,)*] ,) => {
        $crate::json_value::json!(@array [$($values,)*])
    };
    (@object [$($values:expr,)*]) => {
        seekdeep_lossless_json::JsonValue::object([$($values,)*])
    };
    (@object [$($values:expr,)*] $key:literal: null, $($rest:tt)*) => {
        $crate::json_value::json!(@object [$($values,)* ($key, $crate::json_value::json!(null)),] $($rest)*)
    };
    (@object [$($values:expr,)*] $key:literal: [$($value:tt)*], $($rest:tt)*) => {
        $crate::json_value::json!(@object [$($values,)* ($key, $crate::json_value::json!([$($value)*])),] $($rest)*)
    };
    (@object [$($values:expr,)*] $key:literal: {$($value:tt)*}, $($rest:tt)*) => {
        $crate::json_value::json!(@object [$($values,)* ($key, $crate::json_value::json!({$($value)*})),] $($rest)*)
    };
    (@object [$($values:expr,)*] $key:literal: $value:expr, $($rest:tt)*) => {
        $crate::json_value::json!(@object [$($values,)* ($key, $crate::json_value::json!($value)),] $($rest)*)
    };
    (@object [$($values:expr,)*] ,) => {
        $crate::json_value::json!(@object [$($values,)*])
    };
    (null) => { $crate::json_value::null().clone() };
    ([]) => { seekdeep_lossless_json::JsonValue::array(&[]) };
    ({}) => {
        seekdeep_lossless_json::JsonValue::object(std::iter::empty::<(&str, seekdeep_lossless_json::JsonValue)>())
    };
    ([$($values:tt)*]) => { $crate::json_value::json!(@array [] $($values)*,) };
    ({$($values:tt)*}) => { $crate::json_value::json!(@object [] $($values)*,) };
    ($value:expr) => {
        seekdeep_lossless_json::JsonValue::from_serialize(&$value).expect("trajectory value is JSON")
    };
}

pub(crate) use json;
