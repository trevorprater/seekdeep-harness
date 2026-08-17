//! Intrinsic, iterative lossless-JSON snapshots from Boa values.

use std::collections::HashSet;

use boa_engine::{Context, JsObject, JsValue, js_string, property::PropertyKey, value::JsVariant};
use serde_json::{Map, Number, Value};

enum ArenaNode {
    Scalar(Value),
    Array(Vec<usize>),
    Object(Vec<(String, usize)>),
}

enum Destination {
    Root,
    Array(usize),
    Object(usize, String),
}

enum SnapshotTask {
    Visit(JsValue, Destination),
    Leave(JsObject),
}

/// Validates and detaches one Boa value without consulting mutable
/// JavaScript boundary helpers. Returns `None` for `undefined`, lossy scalar
/// types, exotic/prototyped objects, sparse/decorated arrays, cycles, or
/// property access that throws.
pub(crate) fn snapshot_json(value: &JsValue, context: &mut Context) -> Option<Value> {
    let object_prototype = context.intrinsics().constructors().object().prototype();
    let array_prototype = context.intrinsics().constructors().array().prototype();
    let mut arena = Vec::new();
    let mut root = None;
    let mut active = HashSet::new();
    let mut tasks = vec![SnapshotTask::Visit(value.clone(), Destination::Root)];

    while let Some(task) = tasks.pop() {
        match task {
            SnapshotTask::Leave(object) => {
                active.remove(&object);
            }
            SnapshotTask::Visit(candidate, destination) => {
                let node = match candidate.variant() {
                    JsVariant::Null => scalar(&mut arena, Value::Null),
                    JsVariant::Boolean(value) => scalar(&mut arena, Value::Bool(value)),
                    JsVariant::String(value) => {
                        scalar(&mut arena, Value::String(value.to_std_string_escaped()))
                    }
                    JsVariant::Integer32(value) => {
                        scalar(&mut arena, Value::Number(Number::from(value)))
                    }
                    JsVariant::Float64(value)
                        if value.is_finite() && !(value == 0.0 && value.is_sign_negative()) =>
                    {
                        scalar(&mut arena, Value::Number(Number::from_f64(value)?))
                    }
                    JsVariant::Object(object) => {
                        if !active.insert(object.clone()) {
                            return None;
                        }
                        let keys = object.own_property_keys(context).ok()?;
                        let prototype = object.prototype();
                        if object.is_array() {
                            if prototype.as_ref().is_none_or(|prototype| {
                                !JsObject::equals(prototype, &array_prototype)
                            }) {
                                return None;
                            }
                            let length = array_length(&object, &keys)?;
                            let node = arena.len();
                            arena.push(ArenaNode::Array(Vec::with_capacity(length)));
                            tasks.push(SnapshotTask::Leave(object.clone()));
                            for index in (0..length).rev() {
                                let key = PropertyKey::from(u32::try_from(index).ok()?);
                                let value = own_value(&object, &key, context)?;
                                tasks.push(SnapshotTask::Visit(value, Destination::Array(node)));
                            }
                            node
                        } else {
                            if !object.is_ordinary()
                                || prototype.as_ref().is_some_and(|prototype| {
                                    !JsObject::equals(prototype, &object_prototype)
                                })
                            {
                                return None;
                            }
                            let mut properties = Vec::with_capacity(keys.len());
                            for key in keys {
                                let key_string = property_key_string(&key)?;
                                let descriptor = object.borrow().properties().get(&key)?;
                                if descriptor.enumerable() != Some(true) {
                                    return None;
                                }
                                let value = if let Some(value) = descriptor.value() {
                                    value.clone()
                                } else {
                                    object.get(key.clone(), context).ok()?
                                };
                                properties.push((key_string, value));
                            }
                            let node = arena.len();
                            arena.push(ArenaNode::Object(Vec::with_capacity(properties.len())));
                            tasks.push(SnapshotTask::Leave(object.clone()));
                            for (key, value) in properties.into_iter().rev() {
                                tasks.push(SnapshotTask::Visit(
                                    value,
                                    Destination::Object(node, key),
                                ));
                            }
                            node
                        }
                    }
                    JsVariant::Float64(_)
                    | JsVariant::Undefined
                    | JsVariant::BigInt(_)
                    | JsVariant::Symbol(_) => {
                        return None;
                    }
                };
                attach(&mut arena, &mut root, destination, node)?;
            }
        }
    }
    materialize(&arena, root?)
}

fn scalar(arena: &mut Vec<ArenaNode>, value: Value) -> usize {
    let node = arena.len();
    arena.push(ArenaNode::Scalar(value));
    node
}

fn property_key_string(key: &PropertyKey) -> Option<String> {
    match key {
        PropertyKey::String(value) => Some(value.to_std_string_escaped()),
        PropertyKey::Index(value) => Some(value.get().to_string()),
        PropertyKey::Symbol(_) => None,
    }
}

fn array_length(object: &JsObject, keys: &[PropertyKey]) -> Option<usize> {
    let length_key = PropertyKey::from(js_string!("length"));
    let descriptor = object.borrow().properties().get(&length_key)?;
    let length = descriptor.value()?.as_number()?;
    if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
        return None;
    }
    let length = ryu_js::Buffer::new().format(length).parse::<usize>().ok()?;
    if keys.len() != length.checked_add(1)? {
        return None;
    }
    Some(length)
}

fn own_value(object: &JsObject, key: &PropertyKey, context: &mut Context) -> Option<JsValue> {
    let descriptor = object.borrow().properties().get(key)?;
    if descriptor.enumerable() != Some(true) {
        return None;
    }
    if let Some(value) = descriptor.value() {
        Some(value.clone())
    } else {
        object.get(key.clone(), context).ok()
    }
}

fn attach(
    arena: &mut [ArenaNode],
    root: &mut Option<usize>,
    destination: Destination,
    node: usize,
) -> Option<()> {
    match destination {
        Destination::Root => {
            if root.replace(node).is_some() {
                return None;
            }
        }
        Destination::Array(parent) => {
            let ArenaNode::Array(children) = arena.get_mut(parent)? else {
                return None;
            };
            children.push(node);
        }
        Destination::Object(parent, key) => {
            let ArenaNode::Object(children) = arena.get_mut(parent)? else {
                return None;
            };
            children.push((key, node));
        }
    }
    Some(())
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
    use boa_engine::{Source, context::ContextBuilder};
    use serde_json::json;

    use super::*;

    fn evaluated(source: &str) -> (Context, JsValue) {
        let mut context = ContextBuilder::new().build().unwrap();
        let value = context.eval(Source::from_bytes(source)).unwrap();
        (context, value)
    }

    #[test]
    fn accepts_all_lossless_roots_and_deep_values() {
        for (source, expected) in [
            ("null", json!(null)),
            ("true", json!(true)),
            ("1.5", json!(1.5)),
            ("'text'", json!("text")),
            ("[1, { x: false }]", json!([1, { "x": false }])),
            (
                "({ ['__proto__']: 'literal', constructor: [null] })",
                json!({ "__proto__": "literal", "constructor": [null] }),
            ),
        ] {
            let (mut context, value) = evaluated(source);
            assert_eq!(snapshot_json(&value, &mut context), Some(expected));
        }

        let (mut context, value) = evaluated(
            "(() => { let value = 'leaf'; for (let i = 0; i < 3000; i++) value = [value]; return value })()",
        );
        let snapshot = snapshot_json(&value, &mut context).unwrap();
        let mut cursor = &snapshot;
        for _ in 0..3_000 {
            cursor = cursor.as_array().unwrap().first().unwrap();
        }
        assert_eq!(cursor, "leaf");
        std::mem::forget(snapshot);
    }

    #[test]
    fn rejects_every_lossy_or_exotic_shape() {
        for source in [
            "undefined",
            "NaN",
            "Infinity",
            "-0",
            "1n",
            "Symbol('x')",
            "() => 1",
            "new Date()",
            "Object.assign(Object.create({}), { x: 1 })",
            "Object.defineProperty({}, 'hidden', { value: 1 })",
            "Object.defineProperty({}, 'bad', { enumerable: true, get() { throw new Error('no') } })",
            "(() => { const a = {}; a.self = a; return a })()",
            "(() => { const a = []; a.length = 1; return a })()",
            "(() => { const a = [1]; a.extra = true; return a })()",
        ] {
            let (mut context, value) = evaluated(source);
            assert_eq!(
                snapshot_json(&value, &mut context),
                None,
                "accepted {source}"
            );
        }
    }
}
