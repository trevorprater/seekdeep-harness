//! Intrinsic, iterative lossless-JSON snapshots from V8 values.

use std::collections::HashMap;

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

#[derive(Clone)]
pub(crate) struct SnapshotIntrinsics {
    object_prototype: v8::Global<v8::Value>,
    array_prototype: v8::Global<v8::Value>,
}

impl SnapshotIntrinsics {
    pub(crate) fn capture(scope: &mut v8::PinScope) -> Option<Self> {
        let object = v8::Object::new(scope);
        let array = v8::Array::new(scope, 0);
        Some(Self {
            object_prototype: v8::Global::new(scope, object.get_prototype(scope)?),
            array_prototype: v8::Global::new(scope, array.get_prototype(scope)?),
        })
    }
}

enum SnapshotTask<'s> {
    Visit(v8::Local<'s, v8::Value>, Destination),
    ArrayItem(v8::Local<'s, v8::Object>, u32, usize),
    ObjectProperty(
        v8::Local<'s, v8::Object>,
        v8::Local<'s, v8::Value>,
        String,
        usize,
    ),
    Leave(v8::Local<'s, v8::Object>),
}

/// Detaches lossless JSON using engine intrinsics, even after global mutation.
pub(crate) fn snapshot_json<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    intrinsics: &SnapshotIntrinsics,
) -> Option<Value> {
    v8::tc_scope!(let caught, scope);
    snapshot_inner(caught, value, intrinsics)
}

fn snapshot_inner<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    intrinsics: &SnapshotIntrinsics,
) -> Option<Value> {
    let object_prototype = v8::Local::new(scope, &intrinsics.object_prototype);
    let array_prototype = v8::Local::new(scope, &intrinsics.array_prototype);
    let mut arena = Vec::new();
    let mut root = None;
    let mut active: HashMap<i32, Vec<v8::Local<v8::Object>>> = HashMap::new();
    let mut tasks = vec![SnapshotTask::Visit(value, Destination::Root)];
    while let Some(task) = tasks.pop() {
        match task {
            SnapshotTask::Leave(object) => {
                let key = object.get_identity_hash().get();
                let objects = active.get_mut(&key)?;
                let index = objects
                    .iter()
                    .position(|entry| entry.strict_equals(object.into()))?;
                objects.swap_remove(index);
                if objects.is_empty() {
                    active.remove(&key);
                }
            }
            SnapshotTask::ArrayItem(object, index, parent) => {
                let key = v8::String::new(scope, &index.to_string())?;
                if !object.has_own_property(scope, key.into())? {
                    return None;
                }
                let value = object.get_index(scope, index)?;
                tasks.push(SnapshotTask::Visit(value, Destination::Array(parent)));
            }
            SnapshotTask::ObjectProperty(object, key, label, parent) => {
                let value = object.get(scope, key)?;
                tasks.push(SnapshotTask::Visit(
                    value,
                    Destination::Object(parent, label),
                ));
            }
            SnapshotTask::Visit(candidate, destination) => {
                let node = if candidate.is_null() {
                    scalar(&mut arena, Value::Null)
                } else if candidate.is_boolean() {
                    scalar(&mut arena, Value::Bool(candidate.boolean_value(scope)))
                } else if let Ok(text) = v8::Local::<v8::String>::try_from(candidate) {
                    scalar(&mut arena, Value::String(text.to_rust_string_lossy(scope)))
                } else if candidate.is_number() {
                    scalar(&mut arena, snapshot_number(scope, candidate)?)
                } else if candidate.is_object() && !candidate.is_function() {
                    let object = v8::Local::<v8::Object>::try_from(candidate).ok()?;
                    let entries = active.entry(object.get_identity_hash().get()).or_default();
                    if entries.iter().any(|entry| entry.strict_equals(candidate)) {
                        return None;
                    }
                    entries.push(object);
                    let keys = object.get_own_property_names(
                        scope,
                        v8::GetPropertyNamesArgs {
                            property_filter: v8::PropertyFilter::ALL_PROPERTIES,
                            key_conversion: v8::KeyConversionMode::ConvertToString,
                            ..Default::default()
                        },
                    )?;
                    let prototype = object.get_prototype(scope)?;
                    let node = arena.len();
                    tasks.push(SnapshotTask::Leave(object));
                    if let Ok(array) = v8::Local::<v8::Array>::try_from(object) {
                        if !prototype.strict_equals(array_prototype)
                            || keys.length() != array.length().checked_add(1)?
                        {
                            return None;
                        }
                        arena.push(ArenaNode::Array(
                            Vec::with_capacity(array.length() as usize),
                        ));
                        for index in (0..array.length()).rev() {
                            tasks.push(SnapshotTask::ArrayItem(object, index, node));
                        }
                    } else {
                        if !prototype.is_null() && !prototype.strict_equals(object_prototype) {
                            return None;
                        }
                        let properties = enumerable_properties(scope, object, keys)?;
                        arena.push(ArenaNode::Object(Vec::with_capacity(properties.len())));
                        for (key, label) in properties.into_iter().rev() {
                            tasks.push(SnapshotTask::ObjectProperty(object, key, label, node));
                        }
                    }
                    node
                } else {
                    return None;
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

fn snapshot_number(scope: &mut v8::PinScope, candidate: v8::Local<v8::Value>) -> Option<Value> {
    let number = candidate.number_value(scope)?;
    if !number.is_finite() || number == 0.0 && number.is_sign_negative() {
        return None;
    }
    let number = if candidate.is_int32() {
        Number::from(candidate.int32_value(scope)?)
    } else {
        serde_json::from_str(ryu_js::Buffer::new().format(number)).ok()?
    };
    Some(Value::Number(number))
}

fn enumerable_properties<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<v8::Object>,
    keys: v8::Local<v8::Array>,
) -> Option<Vec<(v8::Local<'s, v8::Value>, String)>> {
    let mut properties = Vec::new();
    for index in 0..keys.length() {
        let key = keys.get_index(scope, index)?;
        let text = v8::Local::<v8::String>::try_from(key).ok()?;
        let descriptor = object.get_own_property_descriptor(scope, text.into())?;
        let descriptor = v8::Local::<v8::Object>::try_from(descriptor).ok()?;
        let enumerable = v8::String::new(scope, "enumerable")?;
        if !descriptor.get(scope, enumerable.into())?.is_true() {
            return None;
        }
        properties.push((key, text.to_rust_string_lossy(scope)));
    }
    Some(properties)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn evaluated(source: &str) -> Option<Value> {
        crate::engine::initialize_v8();
        let mut isolate = v8::Isolate::new(v8::CreateParams::default());
        v8::scope!(let handle_scope, &mut isolate);
        let context = v8::Context::new(handle_scope, v8::ContextOptions::default());
        let scope = &mut v8::ContextScope::new(handle_scope, context);
        let intrinsics = SnapshotIntrinsics::capture(scope).unwrap();
        let source = v8::String::new(scope, source).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let value = script.run(scope).unwrap();
        snapshot_json(scope, value, &intrinsics)
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
            assert_eq!(evaluated(source), Some(expected));
        }

        let snapshot = evaluated(
            "(() => { let value = 'leaf'; for (let i = 0; i < 3000; i++) value = [value]; return value })()",
        ).unwrap();
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
            assert_eq!(evaluated(source), None, "accepted {source}");
        }
    }
}
