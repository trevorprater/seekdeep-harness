//! Flat bounded-depth transport for lossless JSON values.

use std::collections::HashSet;

use serde_json::{Map, Number, Value, json};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Encodes one already validated JSON value as a pre-order flat token array.
#[must_use]
pub fn encode_worker_json(value: &Value) -> Value {
    let mut wire = Vec::new();
    let mut pending = vec![value];
    while let Some(current) = pending.pop() {
        match current {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                wire.push(current.clone());
            }
            Value::Array(items) => {
                wire.push(json!({ "kind": "array", "length": items.len() }));
                pending.extend(items.iter().rev());
            }
            Value::Object(object) => {
                let keys = object.keys().cloned().collect::<Vec<_>>();
                wire.push(json!({ "kind": "object", "keys": keys }));
                pending.extend(object.values().rev());
            }
        }
    }
    Value::Array(wire)
}

enum Marker {
    Array(usize),
    Object(Vec<String>),
}

enum ArenaNode {
    Scalar(Value),
    Array(Vec<usize>),
    Object(Vec<(String, usize)>),
}

enum DecodeFrame {
    Array {
        node: usize,
        length: usize,
        index: usize,
    },
    Object {
        node: usize,
        keys: Vec<String>,
        index: usize,
    },
}

impl DecodeFrame {
    fn is_complete(&self) -> bool {
        match self {
            Self::Array { length, index, .. } => index == length,
            Self::Object { keys, index, .. } => *index == keys.len(),
        }
    }
}

/// Rebuilds one JSON value from hostile flat wire input. Malformed, lossy,
/// decorated, incomplete, or trailing traffic returns `None`; traversal does
/// not depend on transported application depth.
#[must_use]
pub fn decode_worker_json(input: &Value) -> Option<Value> {
    let wire = input.as_array()?;
    if wire.is_empty() {
        return None;
    }
    let mut arena = Vec::with_capacity(wire.len());
    let mut frames: Vec<DecodeFrame> = Vec::new();
    let mut root = None;

    for (token_index, token) in wire.iter().enumerate() {
        let (node, frame) = match token {
            Value::Null | Value::Bool(_) | Value::String(_) => {
                let node = arena.len();
                arena.push(ArenaNode::Scalar(token.clone()));
                (node, None)
            }
            Value::Number(number) if valid_number(number) => {
                let node = arena.len();
                arena.push(ArenaNode::Scalar(token.clone()));
                (node, None)
            }
            Value::Object(object) => {
                let marker = parse_marker(object)?;
                let remaining = wire.len() - token_index - 1;
                match marker {
                    Marker::Array(length) if length <= remaining => {
                        let node = arena.len();
                        arena.push(ArenaNode::Array(Vec::with_capacity(length)));
                        let frame = (length > 0).then_some(DecodeFrame::Array {
                            node,
                            length,
                            index: 0,
                        });
                        (node, frame)
                    }
                    Marker::Object(keys) if keys.len() <= remaining => {
                        let node = arena.len();
                        arena.push(ArenaNode::Object(Vec::with_capacity(keys.len())));
                        let frame = (!keys.is_empty()).then_some(DecodeFrame::Object {
                            node,
                            keys,
                            index: 0,
                        });
                        (node, frame)
                    }
                    Marker::Array(_) | Marker::Object(_) => return None,
                }
            }
            Value::Number(_) | Value::Array(_) => return None,
        };

        if let Some(parent) = frames.last_mut() {
            match parent {
                DecodeFrame::Array {
                    node: parent_node,
                    index,
                    ..
                } => {
                    let ArenaNode::Array(children) = &mut arena[*parent_node] else {
                        return None;
                    };
                    children.push(node);
                    *index += 1;
                }
                DecodeFrame::Object {
                    node: parent_node,
                    keys,
                    index,
                } => {
                    let key = keys.get(*index)?.clone();
                    let ArenaNode::Object(children) = &mut arena[*parent_node] else {
                        return None;
                    };
                    children.push((key, node));
                    *index += 1;
                }
            }
        } else if root.replace(node).is_some() {
            return None;
        }
        if let Some(frame) = frame {
            frames.push(frame);
        }
        while frames.last().is_some_and(DecodeFrame::is_complete) {
            frames.pop();
        }
    }
    if !frames.is_empty() {
        return None;
    }
    materialize(&arena, root?)
}

fn valid_number(number: &Number) -> bool {
    number
        .as_f64()
        .is_some_and(|value| value.is_finite() && !(value == 0.0 && value.is_sign_negative()))
}

fn parse_marker(object: &Map<String, Value>) -> Option<Marker> {
    if object.len() != 2 {
        return None;
    }
    match object.get("kind")?.as_str()? {
        "array" => {
            let length = object.get("length")?.as_u64()?;
            if length > MAX_SAFE_INTEGER || object.get("keys").is_some() {
                return None;
            }
            usize::try_from(length).ok().map(Marker::Array)
        }
        "object" => {
            if object.get("length").is_some() {
                return None;
            }
            let values = object.get("keys")?.as_array()?;
            let mut unique = HashSet::with_capacity(values.len());
            let mut keys = Vec::with_capacity(values.len());
            for value in values {
                let key = value.as_str()?.to_owned();
                if !unique.insert(key.clone()) {
                    return None;
                }
                keys.push(key);
            }
            Some(Marker::Object(keys))
        }
        _ => None,
    }
}

enum MaterializeTask {
    Visit(usize),
    Build(usize),
}

fn materialize(arena: &[ArenaNode], root: usize) -> Option<Value> {
    let mut values = vec![None; arena.len()];
    let mut tasks = vec![MaterializeTask::Visit(root)];
    while let Some(task) = tasks.pop() {
        match task {
            MaterializeTask::Visit(index) => match arena.get(index)? {
                ArenaNode::Scalar(value) => values[index] = Some(value.clone()),
                ArenaNode::Array(children) => {
                    tasks.push(MaterializeTask::Build(index));
                    tasks.extend(
                        children
                            .iter()
                            .rev()
                            .map(|child| MaterializeTask::Visit(*child)),
                    );
                }
                ArenaNode::Object(children) => {
                    tasks.push(MaterializeTask::Build(index));
                    tasks.extend(
                        children
                            .iter()
                            .rev()
                            .map(|(_, child)| MaterializeTask::Visit(*child)),
                    );
                }
            },
            MaterializeTask::Build(index) => match arena.get(index)? {
                ArenaNode::Scalar(_) => return None,
                ArenaNode::Array(children) => {
                    let mut array = Vec::with_capacity(children.len());
                    for child in children {
                        array.push(values[*child].take()?);
                    }
                    values[index] = Some(Value::Array(array));
                }
                ArenaNode::Object(children) => {
                    let mut object = Map::with_capacity(children.len());
                    for (key, child) in children {
                        object.insert(key.clone(), values[*child].take()?);
                    }
                    values[index] = Some(Value::Object(object));
                }
            },
        }
    }
    values[root].take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_root_and_preserves_literal_proto_keys() {
        for value in [
            Value::Null,
            json!(true),
            json!(1.5),
            json!("text"),
            json!([1, { "x": false }]),
            json!({ "__proto__": "literal", "constructor": [null] }),
        ] {
            assert_eq!(decode_worker_json(&encode_worker_json(&value)), Some(value));
        }
    }

    #[test]
    fn deep_values_cross_as_flat_wire() {
        let mut value = json!("leaf");
        for _ in 0..3_000 {
            value = Value::Array(vec![value]);
        }
        let wire = encode_worker_json(&value);
        assert_eq!(wire.as_array().unwrap().len(), 3_001);
        assert_eq!(decode_worker_json(&wire), Some(value));
        std::mem::forget(wire);
    }

    #[test]
    fn rejects_malformed_incomplete_lossy_and_decorated_wire() {
        let invalid = [
            json!(null),
            json!([]),
            json!([1, 2]),
            json!([{ "kind": "array", "length": 1 }]),
            json!([{ "kind": "array", "length": -1 }]),
            json!([{ "kind": "array", "length": 0, "extra": true }]),
            json!([{ "kind": "object", "keys": ["x", "x"] }, 1, 2]),
            json!([{ "kind": "object", "keys": [1] }, 1]),
            json!([{ "kind": "unknown", "keys": [] }]),
        ];
        for wire in invalid {
            assert_eq!(decode_worker_json(&wire), None, "accepted {wire}");
        }
    }
}
