//! Retained interpreter objects, released only after their final native owner.

use std::sync::Arc;

use seekdeep_python_sdk::{
    Error, ErrorKind, EventBacking, Notification, NotificationBacking, NotificationData,
    ObjectHandle, Result, RunEvent,
};
use serde_json::{Map, Value, json};

use crate::Callback;

pub(crate) struct Object {
    callback: Callback,
    handle: ObjectHandle,
}

impl Object {
    pub(crate) fn new(callback: Callback, value: Value) -> Result<Arc<Self>> {
        let handle: ObjectHandle = serde_json::from_value(value).map_err(|error| {
            Error::new(
                ErrorKind::Type,
                format!("invalid interpreter object handle: {error}"),
            )
        })?;
        if handle.owner != callback.context.0 {
            return Err(Error::new(
                ErrorKind::Type,
                "interpreter object belongs to another callback owner",
            ));
        }
        Ok(Arc::new(Self { callback, handle }))
    }

    pub(crate) fn handle(&self) -> ObjectHandle {
        self.handle
    }

    pub(crate) fn invoke(&self, operation: &str, arguments: Value) -> Result<Value> {
        let mut payload = json!({"object":self.handle.value});
        payload["arguments"] = arguments;
        self.callback.invoke(operation, payload)
    }
}

struct ContextPin(Callback);
impl Drop for ContextPin {
    fn drop(&mut self) {
        let _ = self.0.invoke("context.release", Value::Null);
    }
}

pub(crate) fn reader_lifetime(callback: Callback) -> Result<Arc<dyn std::any::Any + Send + Sync>> {
    callback.invoke("context.retain", Value::Null)?;
    Ok(Arc::new(ContextPin(callback)))
}

impl Drop for Object {
    fn drop(&mut self) {
        let _ = self
            .callback
            .invoke("object.release", json!({"object":self.handle.value}));
    }
}

struct ForeignNotification(Arc<Object>);

impl NotificationBacking for ForeignNotification {
    fn read(&self) -> Result<NotificationData> {
        serde_json::from_value(self.0.invoke("notification.read", Value::Null)?).map_err(|error| {
            Error::new(
                ErrorKind::Type,
                format!("invalid notification fields: {error}"),
            )
        })
    }

    fn replace(&self, value: NotificationData) -> Result<()> {
        self.0.invoke(
            "notification.replace",
            serde_json::to_value(value)
                .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))?,
        )?;
        Ok(())
    }

    fn event(&self) -> Result<Option<RunEvent>> {
        let value = self.0.invoke("notification.event", Value::Null)?;
        let object = Object::new(self.0.callback, value)?;
        if object
            .invoke("object.is_dictionary", Value::Null)?
            .as_bool()
            != Some(true)
        {
            return Ok(None);
        }
        Ok(Some(RunEvent::from_backing(Arc::new(ForeignEvent(object)))))
    }

    fn object_handle(&self) -> Option<ObjectHandle> {
        Some(self.0.handle())
    }
}

struct ForeignEvent(Arc<Object>);
impl EventBacking for ForeignEvent {
    fn read(&self) -> Result<Map<String, Value>> {
        self.0
            .invoke("object.read", Value::Null)?
            .as_object()
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Type, "retained event is not a dictionary"))
    }

    fn replace(&self, value: Map<String, Value>) -> Result<()> {
        self.0.invoke("object.replace", Value::Object(value))?;
        Ok(())
    }

    fn object_handle(&self) -> Option<ObjectHandle> {
        Some(self.0.handle())
    }
}

pub(crate) fn notification_from_message(
    callback: Callback,
    message: &Object,
    value: NotificationData,
    original_payload: bool,
) -> Result<Notification> {
    let mut arguments = json!({"original_payload":original_payload});
    arguments["method"] = Value::String(value.method);
    arguments["payload"] = Value::Object(value.payload);
    let value = message.invoke("message.notification", arguments)?;
    Ok(Notification::from_backing(Arc::new(ForeignNotification(
        Object::new(callback, value)?,
    ))))
}

pub(crate) fn notification(callback: Callback, value: NotificationData) -> Result<Notification> {
    let value = callback.invoke(
        "notification.create",
        serde_json::to_value(value)
            .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))?,
    )?;
    Ok(Notification::from_backing(Arc::new(ForeignNotification(
        Object::new(callback, value)?,
    ))))
}
