//! Cross-module conformance against direct pinned `CosmoKit` source oracles.

use chrono::{Datelike as _, Local, TimeZone as _, Timelike as _};
use indexmap::IndexMap;
use seekdeep_cosmokit::{array, misc, string, time, types};
use serde_json::{Value, json};

#[test]
fn array_and_ordered_map_helpers_match_source_order_and_presence() {
    assert!(array::contain(&[1, 2, 3], &[3, 1]));
    assert_eq!(array::intersection(&[2, 1, 2, 3], &[2, 3]), [2, 2, 3]);
    assert_eq!(array::difference(&[2, 1, 2, 3], &[2]), [1, 3]);
    assert_eq!(array::union(&[2, 1, 2], &[3, 1]), [2, 1, 3]);
    assert_eq!(
        array::make_array_source::<i32>(array::MaybeArray::Nullish),
        Vec::<i32>::new()
    );
    assert_eq!(array::make_array_source(array::MaybeArray::Scalar(1)), [1]);
    assert_eq!(
        array::make_array_source(array::MaybeArray::Array(vec![1, 2])),
        [1, 2]
    );

    let values = IndexMap::from([("a", Some(1)), ("b", None), ("c", Some(3))]);
    assert_eq!(
        misc::pick_optional(&values, ["b", "a"], false),
        IndexMap::from([("a", Some(1))])
    );
    let forced = misc::pick_optional(&values, ["b"], true);
    assert!(forced.contains_key("b"));
    assert_eq!(forced["b"], None);
    assert_eq!(
        misc::omit(&IndexMap::from([("a", 1), ("b", 2)]), ["a"]),
        IndexMap::from([("b", 2)])
    );
    assert!(misc::is_plain_object(&json!({})));
    assert!(!misc::is_plain_object(&json!([])));
    misc::noop();
}

#[test]
fn string_tokenization_property_and_path_oracles_match_exactly() {
    assert_eq!(string::camel_case("foo-bar_baz"), "fooBarBaz");
    assert_eq!(string::camel_case("foo-1"), "foo-1");
    assert_eq!(string::param_case("XMLHttpRequest"), "xml-http-request");
    assert_eq!(string::snake_case("XMLHttpRequest"), "xml_http_request");
    assert_eq!(string::param_case("foo-Bar_BAZ"), "foo-bar-baz");
    assert_eq!(string::param_case("foo--bar"), "foo-bar");
    assert_eq!(string::param_case("éValue"), "évalue");
    assert_eq!(string::format_property("alpha_$1"), ".alpha_$1");
    assert_eq!(string::format_property("é"), "[\"é\"]");
    assert_eq!(string::format_property("bad-key"), "[\"bad-key\"]");
    assert_eq!(string::trim_slash("foo//"), "foo/");
    assert_eq!(string::sanitize(""), "");
    assert_eq!(string::sanitize("foo/"), "/foo");
    assert_eq!(string::sanitize("foo//"), "/foo/");
}

#[test]
fn time_relative_format_template_and_date_paths_match_source() {
    assert!((time::parse_time("1week2days3h4min5sec") - 788_645_000.0).abs() < f64::EPSILON);
    assert!(time::parse_time("1h2d").abs() < f64::EPSILON);
    assert_eq!(time::format(999.0), "999ms");
    assert_eq!(time::format(1_500.0), "2s");
    assert_eq!(time::format(-1_500.0), "-1s");
    assert_eq!(time::to_digits(3, 2), "03");
    let now = Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .unwrap();
    assert_eq!(time::parse_date("", now), Some(now));
    assert_eq!(time::parse_date("2h", now).unwrap().hour(), 5);
    assert_eq!(time::parse_date("12:30", now).unwrap().day(), 2);
    assert_eq!(time::parse_date("12:30", now).unwrap().minute(), 30);
    assert_eq!(time::parse_date("8-25-12:30", now).unwrap().month(), 8);
    assert_eq!(
        time::parse_date("2025-04-03", now)
            .unwrap()
            .timestamp_millis(),
        chrono::DateTime::parse_from_rfc3339("2025-04-03T00:00:00Z")
            .unwrap()
            .timestamp_millis()
    );
}

#[test]
fn binary_clone_and_nullable_equality_match_native_source_behavior() {
    let bytes = [0, 1, 254, 255];
    assert_eq!(types::binary::to_hex(&bytes), "0001feff");
    assert_eq!(types::binary::from_hex("01gg02"), Ok(vec![1]));
    assert_eq!(types::binary::to_base64(b"hi"), "aGk=");
    assert_eq!(types::binary::from_base64("a!Gk="), Ok(b"hi".to_vec()));
    let value = json!([1, {"a":[2,3]}]);
    assert_eq!(types::clone_json(&value), value);
    assert!(types::deep_equal_json(None, Some(&Value::Null), false));
    assert!(!types::deep_equal_json(None, Some(&Value::Null), true));
}
