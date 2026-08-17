//! Exact behavioral mirror of `packages/core/tools/tests/properties.spec.ts`.

use std::{collections::BTreeMap, panic::AssertUnwindSafe};

use proptest::{
    collection::{btree_map, vec},
    prelude::*,
};
use seekdeep_tools::{parameter_schema_spec_to_json_schema, validate_args};
use serde_json::{Map, Value, json};

fn required(mut schema: Value, required: bool) -> Value {
    if required {
        schema
            .as_object_mut()
            .expect("generated schema object")
            .insert("required".to_owned(), Value::Bool(true));
    }
    schema
}

fn leaf_property() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(|is_required| required(json!({"type": "string"}), is_required)),
        any::<bool>().prop_map(|is_required| required(json!({"type": "number"}), is_required)),
        any::<bool>().prop_map(|is_required| required(json!({"type": "integer"}), is_required)),
        any::<bool>().prop_map(|is_required| required(json!({"type": "boolean"}), is_required)),
        any::<bool>().prop_map(|is_required| required(json!({"type": "null"}), is_required)),
        any::<bool>().prop_map(|is_required| required(json!({"type": "json"}), is_required)),
        (vec("[A-Za-z0-9_]{1,8}", 1..=3), any::<bool>(),).prop_map(|(values, is_required)| {
            required(json!({"type": "string", "enum": values}), is_required)
        }),
        ("[A-Za-z0-9_]{0,8}", any::<bool>()).prop_map(|(value, is_required)| required(
            json!({"type": "string", "const": value}),
            is_required,
        )),
        any::<bool>().prop_map(|is_required| required(
            json!({"oneOf": [{"type": "string"}, {"type": "null"}]}),
            is_required,
        )),
    ]
}

fn parameter_schema(depth: u32) -> BoxedStrategy<Value> {
    let property = property_schema(depth);
    btree_map("[A-Za-z_][A-Za-z0-9_]{0,5}", property, 0..=4)
        .prop_map(map_value)
        .boxed()
}

fn property_schema(depth: u32) -> BoxedStrategy<Value> {
    if depth == 0 {
        return leaf_property().boxed();
    }
    let nested = parameter_schema(depth - 1);
    let item = property_schema(depth - 1).prop_map(strip_required);
    prop_oneof![
        3 => leaf_property(),
        1 => (nested, any::<bool>(), any::<bool>()).prop_map(
            |(properties, is_required, additional_properties)| required(
                json!({
                    "type": "object",
                    "properties": properties,
                    "additionalProperties": additional_properties,
                }),
                is_required,
            )
        ),
        1 => (item, any::<bool>()).prop_map(|(items, is_required)| required(
            json!({"type": "array", "items": items}),
            is_required,
        )),
    ]
    .boxed()
}

fn strip_required(mut schema: Value) -> Value {
    schema
        .as_object_mut()
        .expect("generated schema object")
        .remove("required");
    schema
}

fn map_value(entries: BTreeMap<String, Value>) -> Value {
    Value::Object(entries.into_iter().collect())
}

fn json_value() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i32>().prop_map(|value| json!(value)),
        "[A-Za-z0-9_ ]{0,12}".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 64, 4, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..=4).prop_map(Value::Array),
            btree_map("[A-Za-z_][A-Za-z0-9_]{0,5}", inner, 0..=4).prop_map(map_value),
        ]
    })
    .boxed()
}

fn required_keys(spec: &Value) -> Vec<&str> {
    spec.as_object()
        .expect("parameter map")
        .iter()
        .filter_map(|(key, property)| {
            (property.get("required") == Some(&Value::Bool(true))).then_some(key.as_str())
        })
        .collect()
}

fn assert_required_at_every_level(spec: &Value, compiled: &Value) {
    let mut actual = compiled
        .get("required")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |values| {
            values.iter().filter_map(Value::as_str).collect::<Vec<_>>()
        });
    let mut expected = required_keys(spec);
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected);

    for (key, property) in spec.as_object().expect("parameter map") {
        if property.get("type") == Some(&Value::String("object".to_owned()))
            && let Some(properties) = property.get("properties")
        {
            assert_required_at_every_level(properties, &compiled["properties"][key]);
        }
    }
}

fn satisfying_value(schema: &Value) -> Value {
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        return satisfying_value(&branches[0]);
    }
    if let Some(constant) = schema.get("const") {
        return constant.clone();
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return value.clone();
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => json!("value"),
        Some("number") => json!(0.5),
        Some("integer") => json!(0),
        Some("boolean") => json!(false),
        Some("null") => Value::Null,
        Some("array") => schema.get("items").map_or_else(
            || Value::Array(Vec::new()),
            |item| json!([satisfying_value(item)]),
        ),
        Some("object") => satisfying_args(schema.get("properties").unwrap_or(&Value::Null)),
        Some("json") | None => json!({"lossless": true}),
        Some(other) => panic!("unexpected generated type {other}"),
    }
}

fn satisfying_args(spec: &Value) -> Value {
    let Some(properties) = spec.as_object() else {
        return Value::Object(Map::new());
    };
    Value::Object(
        properties
            .iter()
            .map(|(key, property)| (key.clone(), satisfying_value(property)))
            .collect(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn json_schema_required_equals_required_true_keys_at_every_level(spec in parameter_schema(2)) {
        let compiled = parameter_schema_spec_to_json_schema(spec.clone()).expect("generated schema compiles");
        assert_required_at_every_level(&spec, compiled.as_value());
    }

    #[test]
    fn conversion_is_total_for_any_generated_spec(spec in parameter_schema(3)) {
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            parameter_schema_spec_to_json_schema(spec)
        }));
        prop_assert!(result.is_ok());
        prop_assert!(result.expect("compiler must not panic").is_ok());
    }

    #[test]
    fn validate_args_is_total_for_any_spec_and_any_structural_json_input(
        spec in parameter_schema(2),
        args in json_value(),
    ) {
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| validate_args(spec, &args)));
        prop_assert!(result.is_ok());
        prop_assert!(result.expect("validator must not panic").is_ok());
    }

    #[test]
    fn args_satisfying_the_spec_pass_validate_args(spec in parameter_schema(2)) {
        let args = satisfying_args(&spec);
        prop_assert_eq!(validate_args(spec, &args).expect("generated schema compiles"), Vec::<String>::new());
    }

    #[test]
    fn dropping_a_required_key_is_always_rejected(
        spec in parameter_schema(1).prop_filter("needs a required property", |spec| !required_keys(spec).is_empty()),
    ) {
        let victim = required_keys(&spec)[0].to_owned();
        let mut broken = satisfying_args(&spec);
        broken.as_object_mut().expect("arguments object").remove(&victim);
        let violations = validate_args(spec, &broken).expect("generated schema compiles");
        let quoted_victim = format!("\"{victim}\"");
        prop_assert!(violations.iter().any(|violation| violation.contains(&quoted_victim)));
    }

    #[test]
    fn a_non_object_top_level_is_always_rejected(
        spec in parameter_schema(1),
        not_an_object in prop_oneof![
            "[A-Za-z0-9_ ]{0,12}".prop_map(Value::String),
            any::<i32>().prop_map(|value| json!(value)),
            any::<bool>().prop_map(Value::Bool),
            Just(Value::Null),
            vec(json_value(), 0..=3).prop_map(Value::Array),
        ],
    ) {
        let violations = validate_args(spec, &not_an_object).expect("generated schema compiles");
        prop_assert!(!violations.is_empty());
    }
}
