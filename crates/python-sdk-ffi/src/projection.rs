//! Interpreter-object traversal for Python projection helpers.

use std::sync::Arc;

use seekdeep_python_sdk::{Error, ErrorKind, ObjectHandle, Result};
use serde_json::{Value, json};

use crate::{Callback, Reply, objects::Object};

struct Projection {
    callback: Callback,
}

impl Projection {
    fn new(callback: Callback, root: ObjectHandle) -> Result<(Self, Arc<Object>)> {
        let callback = callback.with_owner(root.owner);
        let root = Object::new(callback, json!(root))?;
        Ok((Self { callback }, root))
    }

    fn object(&self, value: Value) -> Result<Arc<Object>> {
        Object::new(self.callback, value)
    }

    fn boolean(object: &Object, operation: &str) -> Result<bool> {
        object
            .invoke(operation, Value::Null)?
            .as_bool()
            .ok_or_else(|| Error::new(ErrorKind::Type, "interpreter test returned no boolean"))
    }

    fn dictionary(object: &Object) -> Result<bool> {
        Self::boolean(object, "object.is_dictionary")
    }

    fn list(object: &Object) -> Result<bool> {
        Self::boolean(object, "object.is_list")
    }

    fn string(object: &Object) -> Result<bool> {
        Self::boolean(object, "object.is_string")
    }

    fn truth(object: &Object) -> Result<bool> {
        Self::boolean(object, "object.truth")
    }

    fn equals(object: &Object, expected: &str) -> Result<bool> {
        object
            .invoke("object.equals", json!(expected))?
            .as_bool()
            .ok_or_else(|| Error::new(ErrorKind::Type, "interpreter equality returned no boolean"))
    }

    fn not_equals(object: &Object, expected: &str) -> Result<bool> {
        object
            .invoke("object.not_equals", json!(expected))?
            .as_bool()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Type,
                    "interpreter inequality returned no boolean",
                )
            })
    }

    fn get(&self, object: &Object, key: &str) -> Result<Arc<Object>> {
        self.object(object.invoke("object.get", json!(key))?)
    }

    fn sequence(&self, object: &Object, operation: &str) -> Result<Vec<Arc<Object>>> {
        let handles: Vec<ObjectHandle> =
            serde_json::from_value(object.invoke(operation, Value::Null)?)
                .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))?;
        handles
            .into_iter()
            .map(|handle| self.object(json!(handle)))
            .collect()
    }

    fn render(&self, object: &Object) -> Result<Arc<Object>> {
        self.object(object.invoke("object.string", Value::Null)?)
    }

    fn none(&self) -> Result<Arc<Object>> {
        self.object(self.callback.invoke("object.none", Value::Null)?)
    }

    fn concat(&self, values: &[Arc<Object>]) -> Result<Reply> {
        let handles = values
            .iter()
            .map(|value| value.handle())
            .collect::<Vec<_>>();
        let joined = self.object(self.callback.invoke("objects.concat", json!(handles))?)?;
        Ok(Reply::object(joined.handle(), joined))
    }
}

pub(crate) fn final_response(callback: Callback, events: ObjectHandle) -> Result<Reply> {
    let (projection, events) = Projection::new(callback, events)?;
    for event in projection.sequence(&events, "object.reversed")? {
        if !Projection::dictionary(&event)?
            || Projection::not_equals(
                projection.get(&event, "type")?.as_ref(),
                "assistant/message",
            )?
        {
            continue;
        }
        let data = projection.get(&event, "data")?;
        if !Projection::dictionary(&data)? {
            continue;
        }
        let message = projection.get(&data, "message")?;
        let owner = if Projection::dictionary(&message)? {
            message
        } else {
            Arc::clone(&data)
        };
        let content = projection.get(&owner, "content")?;
        if !Projection::list(&content)? {
            continue;
        }
        let mut text = Vec::new();
        for block in projection.sequence(&content, "object.iter")? {
            if !Projection::dictionary(&block)?
                || !Projection::equals(projection.get(&block, "type")?.as_ref(), "text")?
            {
                continue;
            }
            let value = projection.get(&block, "text")?;
            if Projection::truth(&value)? {
                text.push(projection.render(&value)?);
            }
        }
        return projection.concat(&text);
    }
    projection.concat(&[])
}

pub(crate) fn finish_reason(callback: Callback, events: ObjectHandle) -> Result<Reply> {
    let (projection, events) = Projection::new(callback, events)?;
    for event in projection.sequence(&events, "object.reversed")? {
        if !Projection::dictionary(&event)?
            || Projection::not_equals(projection.get(&event, "type")?.as_ref(), "turn/end")?
        {
            continue;
        }
        let data = projection.get(&event, "data")?;
        let reason = if Projection::dictionary(&data)? {
            projection.get(&data, "reason")?
        } else {
            projection.none()?
        };
        let kind = if Projection::dictionary(&reason)? {
            projection.get(&reason, "kind")?
        } else {
            projection.none()?
        };
        if !Projection::string(&kind)? {
            return Err(Error::new(
                ErrorKind::Protocol,
                "turn/end event requires a string data.reason.kind",
            ));
        }
        return Ok(Reply::object(kind.handle(), kind));
    }
    Ok(Reply::json(Value::Null))
}
