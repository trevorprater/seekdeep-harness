//! Shared notification and captured-event identities across native and foreign consumers.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{NotificationData, Result};

/// Opaque value identity in an independently owned interpreter context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectHandle {
    /// Interpreter owner that retains this value.
    pub owner: u64,
    /// Value identity within that owner.
    pub value: u64,
}

/// An event object whose lifetime is independent of its notification's current payload.
pub trait EventBacking: Send + Sync {
    /// Reads the current dictionary contents.
    ///
    /// # Errors
    /// Propagates foreign-object access failures.
    fn read(&self) -> Result<Map<String, Value>>;

    /// Replaces dictionary contents without changing object identity.
    ///
    /// # Errors
    /// Propagates foreign-object mutation failures.
    fn replace(&self, value: Map<String, Value>) -> Result<()>;

    /// Retained interpreter identity, when this event belongs to a foreign runtime.
    fn object_handle(&self) -> Option<ObjectHandle> {
        None
    }
}

/// One shared root-session event dictionary.
#[derive(Clone)]
pub struct RunEvent(Arc<dyn EventBacking>);

impl RunEvent {
    /// Owns a native dictionary with shared identity.
    pub fn new(value: Map<String, Value>) -> Self {
        Self(Arc::new(NativeEvent(Mutex::new(value))))
    }

    /// Adopts an object whose backing owns its foreign reference and release.
    pub fn from_backing(backing: Arc<dyn EventBacking>) -> Self {
        Self(backing)
    }

    /// Reads current event contents.
    ///
    /// # Errors
    /// Propagates a backing access failure.
    pub fn read(&self) -> Result<Map<String, Value>> {
        self.0.read()
    }

    /// Changes contents while preserving every retained reference.
    ///
    /// # Errors
    /// Propagates a backing mutation failure.
    pub fn replace(&self, value: Map<String, Value>) -> Result<()> {
        self.0.replace(value)
    }

    /// Foreign object identity, if present.
    pub fn object_handle(&self) -> Option<ObjectHandle> {
        self.0.object_handle()
    }

    /// Whether both handles retain the same backing object.
    pub fn same_object(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

struct NativeEvent(Mutex<Map<String, Value>>);
impl EventBacking for NativeEvent {
    fn read(&self) -> Result<Map<String, Value>> {
        Ok(self.0.lock().clone())
    }
    fn replace(&self, value: Map<String, Value>) -> Result<()> {
        *self.0.lock() = value;
        Ok(())
    }
}

/// A notification object shared by every matching subscription.
pub trait NotificationBacking: Send + Sync {
    /// Reads the current method and payload.
    ///
    /// # Errors
    /// Propagates foreign-object access failures.
    fn read(&self) -> Result<NotificationData>;

    /// Replaces the notification's fields, including its current event reference.
    ///
    /// # Errors
    /// Propagates foreign-object mutation failures.
    fn replace(&self, value: NotificationData) -> Result<()>;

    /// Captures the current event dictionary; later field replacement does not retarget it.
    ///
    /// # Errors
    /// Propagates foreign-object access failures.
    fn event(&self) -> Result<Option<RunEvent>>;

    /// Retained interpreter identity, when this notification belongs to a foreign runtime.
    fn object_handle(&self) -> Option<ObjectHandle> {
        None
    }
}

/// Shared mutable notification, not a copy of its latest JSON contents.
#[derive(Clone)]
pub struct Notification(Arc<dyn NotificationBacking>);

impl Notification {
    /// Creates a native notification and captures its initial event object.
    pub fn new(value: NotificationData) -> Self {
        Self(Arc::new(NativeNotification(Mutex::new(
            NativeNotificationState::new(value),
        ))))
    }

    /// Adopts an object whose backing owns its foreign reference and release.
    pub fn from_backing(backing: Arc<dyn NotificationBacking>) -> Self {
        Self(backing)
    }

    /// Reads the notification's current fields.
    ///
    /// # Errors
    /// Propagates a backing access failure.
    pub fn read(&self) -> Result<NotificationData> {
        self.0.read()
    }

    /// Replaces fields without changing the notification object's identity.
    ///
    /// # Errors
    /// Propagates a backing mutation failure.
    pub fn replace(&self, value: NotificationData) -> Result<()> {
        self.0.replace(value)
    }

    /// Retains the event dictionary currently selected by the payload.
    ///
    /// # Errors
    /// Propagates a backing access failure.
    pub fn event(&self) -> Result<Option<RunEvent>> {
        self.0.event()
    }

    /// Foreign object identity, if present.
    pub fn object_handle(&self) -> Option<ObjectHandle> {
        self.0.object_handle()
    }

    /// Whether both handles retain the same backing object.
    pub fn same_object(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

struct NativeNotificationState {
    value: NotificationData,
    event: Option<RunEvent>,
}

impl NativeNotificationState {
    fn new(value: NotificationData) -> Self {
        let event = value
            .payload
            .get("event")
            .and_then(Value::as_object)
            .cloned()
            .map(RunEvent::new);
        Self { value, event }
    }
}

struct NativeNotification(Mutex<NativeNotificationState>);
impl NotificationBacking for NativeNotification {
    fn read(&self) -> Result<NotificationData> {
        let (mut value, event) = {
            let state = self.0.lock();
            (state.value.clone(), state.event.clone())
        };
        if let Some(event) = event {
            value
                .payload
                .insert("event".to_owned(), Value::Object(event.read()?));
        }
        Ok(value)
    }

    fn replace(&self, value: NotificationData) -> Result<()> {
        *self.0.lock() = NativeNotificationState::new(value);
        Ok(())
    }

    fn event(&self) -> Result<Option<RunEvent>> {
        Ok(self.0.lock().event.clone())
    }
}

macro_rules! snapshot_traits {
    ($name:ident, $snapshot:ty, $constructor:path) => {
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                self.read()
                    .map_err(serde::ser::Error::custom)?
                    .serialize(serializer)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                <$snapshot>::deserialize(deserializer).map($constructor)
            }
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.read().fmt(formatter)
            }
        }
        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                match (self.read(), other.read()) {
                    (Ok(left), Ok(right)) => left == right,
                    _ => false,
                }
            }
        }
    };
}

snapshot_traits!(Notification, NotificationData, Notification::new);
snapshot_traits!(RunEvent, Map<String, Value>, RunEvent::new);
