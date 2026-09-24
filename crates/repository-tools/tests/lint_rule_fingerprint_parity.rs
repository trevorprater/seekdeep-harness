//! Exact target Oxlint override/profile fingerprints from the pinned source audit.

use std::path::Path;

use indexmap::IndexMap;
use seekdeep_repository_tools::clean::parse_jsonc_value;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

#[derive(Clone, Copy)]
struct Profile {
    count: usize,
    indexes: &'static [usize],
    sha256: &'static str,
}

const PROFILES: &[(&str, Profile)] = &[
    (
        "source",
        Profile {
            count: 88,
            indexes: &[0, 1, 4, 5],
            sha256: "da1dfd77cb6eb66be93d8d3820f9b9b68b7aa391c24680f8851c0910298f9e3b",
        },
    ),
    (
        "example",
        Profile {
            count: 87,
            indexes: &[0, 1, 2, 4, 5],
            sha256: "6a2606053bc1ec1de3b02611de88ea51d201dac13a1f193e4934d33c08b95f08",
        },
    ),
    (
        "test",
        Profile {
            count: 83,
            indexes: &[0, 3, 4, 5],
            sha256: "7995e14926a36c40bd65c474637735222a95fb030395681685f03060e50a7b78",
        },
    ),
];

#[test]
fn every_override_and_rule_profile_fingerprint_is_exact() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config =
        parse_jsonc_value(&std::fs::read_to_string(root.join(".oxlintrc.json")).unwrap()).unwrap();
    let overrides = config["overrides"].as_array().unwrap();
    assert_eq!(overrides.len(), 8);
    for (name, profile) in PROFILES {
        let rules = merged_rules(overrides, profile.indexes);
        assert_eq!(rules.len(), profile.count, "{name}");
        let encoded = serde_json::to_vec(&rules).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(encoded)),
            profile.sha256,
            "{name}"
        );
    }
}

fn merged_rules(overrides: &[Value], indexes: &[usize]) -> IndexMap<String, Value> {
    let mut merged = IndexMap::<String, Value>::new();
    for index in indexes {
        let rules = overrides[*index]["rules"].as_object().unwrap();
        for (name, value) in rules {
            merged.insert(name.clone(), value.clone());
        }
    }
    let mut enabled = merged
        .into_iter()
        .filter_map(|(name, value)| {
            let severity = severity(&value);
            if severity == 0 {
                return None;
            }
            let options = value
                .as_array()
                .map(|value| value.iter().skip(1).cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            let normalized = std::iter::once(Value::from(severity))
                .chain(options)
                .collect::<Vec<_>>();
            Some((name, Value::Array(normalized)))
        })
        .collect::<Vec<_>>();
    enabled.sort_by(|left, right| left.0.cmp(&right.0));
    enabled.into_iter().collect()
}

fn severity(value: &Value) -> u8 {
    let level = value
        .as_array()
        .and_then(|value| value.first())
        .unwrap_or(value);
    match level {
        Value::String(level) if level == "off" => 0,
        Value::String(level) if matches!(level.as_str(), "warn" | "warning") => 1,
        Value::String(level) if level == "error" => 2,
        Value::Number(level) if level.as_u64().is_some_and(|level| level <= 2) => {
            u8::try_from(level.as_u64().unwrap()).unwrap()
        }
        _ => panic!("unsupported lint severity: {level}"),
    }
}
