//! Per-session browser directory resolver and Session-scope lifecycle ownership.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use js_sys::{Function, Object};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsValue, closure::Closure};
use wasm_bindgen_futures::spawn_local;

use super::{
    WasmModelDirectory, browser_directory::BrowserTransport, call_method, object, required, set,
    translated,
};
use crate::ModelDirectory;

#[derive(Clone)]
struct DirectoryRecord {
    directory: Rc<ModelDirectory>,
    face: JsValue,
}

/// Root owner of lazy per-session model directories.
pub(crate) struct BrowserModelDirectoryResolver {
    ctx: JsValue,
    sessions: JsValue,
    transport: Rc<BrowserTransport>,
    translate: Function,
    directories: Rc<RefCell<BTreeMap<SessionId, DirectoryRecord>>>,
}

impl BrowserModelDirectoryResolver {
    pub(crate) fn new(
        ctx: JsValue,
        sessions: JsValue,
        sessions_api: JsValue,
        translate: Function,
    ) -> Rc<Self> {
        Rc::new(Self {
            ctx,
            sessions,
            transport: BrowserTransport::new(sessions_api),
            translate,
            directories: Rc::new(RefCell::new(BTreeMap::new())),
        })
    }

    pub(crate) fn face(self: &Rc<Self>) -> Result<JsValue, JsValue> {
        let face = Object::new();
        let resolver = self.clone();
        let directory_for = Closure::wrap(Box::new(move |session_id: String| {
            resolver.directory_face(&SessionId::new(session_id))
        })
            as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
        set(&face, "directoryFor", &directory_for.into_js_value())?;
        Ok(face.into())
    }

    pub(crate) fn directory(
        self: &Rc<Self>,
        session_id: &SessionId,
    ) -> Result<Rc<ModelDirectory>, JsValue> {
        self.ensure(session_id).map(|record| record.directory)
    }

    pub(crate) fn face_for(self: &Rc<Self>, session_id: &SessionId) -> Result<JsValue, JsValue> {
        self.directory_face(session_id)
    }

    pub(crate) fn reset_all(&self) {
        let directories = self
            .directories
            .borrow()
            .values()
            .map(|record| record.directory.clone())
            .collect::<Vec<_>>();
        for directory in directories {
            if let Some(future) = directory.reset_connected() {
                spawn_local(async move {
                    let _ = future.await;
                });
            }
        }
    }

    pub(crate) fn refresh_all(&self) {
        let directories = self
            .directories
            .borrow()
            .values()
            .map(|record| record.directory.clone())
            .collect::<Vec<_>>();
        for directory in directories {
            spawn_local(async move {
                let _ = directory.load().await;
            });
        }
    }

    pub(crate) fn dispose_all(&self) {
        let directories = std::mem::take(&mut *self.directories.borrow_mut());
        for record in directories.into_values() {
            record.directory.dispose();
        }
    }

    fn directory_face(self: &Rc<Self>, session_id: &SessionId) -> Result<JsValue, JsValue> {
        self.ensure(session_id).map(|record| record.face)
    }

    fn ensure(self: &Rc<Self>, session_id: &SessionId) -> Result<DirectoryRecord, JsValue> {
        if let Some(record) = self.directories.borrow().get(session_id) {
            return Ok(record.clone());
        }
        let scope = call_method(
            &self.sessions,
            "scope",
            &[JsValue::from_str(session_id.as_str())],
        )?;
        if scope.is_null() || scope.is_undefined() {
            return Err(js_sys::Error::new(&format!(
                "ui-model-selection: session {:?} resolved no scope",
                session_id.as_str()
            ))
            .into());
        }
        let available_sessions = self.sessions.clone();
        let available_id = session_id.clone();
        let available = Rc::new(move || {
            call_method(
                &available_sessions,
                "subagentAddress",
                &[JsValue::from_str(available_id.as_str())],
            )
            .is_ok_and(|address| address.is_undefined())
        });
        let directory = ModelDirectory::new(self.transport.clone(), session_id.clone(), available);
        let face: JsValue = WasmModelDirectory::from_directory(directory.clone())?.into();
        let record = DirectoryRecord {
            directory: directory.clone(),
            face,
        };
        self.directories
            .borrow_mut()
            .insert(session_id.clone(), record.clone());

        if let Err(error) = self.own_composer_block(&scope, session_id, &directory) {
            self.directories.borrow_mut().remove(session_id);
            directory.dispose();
            return Err(error);
        }
        if let Err(error) = self.own_directory(&scope, session_id, &directory) {
            self.directories.borrow_mut().remove(session_id);
            directory.dispose();
            return Err(error);
        }
        Ok(record)
    }

    fn own_composer_block(
        &self,
        scope: &JsValue,
        session_id: &SessionId,
        directory: &Rc<ModelDirectory>,
    ) -> Result<(), JsValue> {
        let conversation = call_method(&self.ctx, "get", &[JsValue::from_str("conversation")])?;
        if conversation.is_null() || conversation.is_undefined() {
            return Ok(());
        }
        publish_block(&conversation, session_id, directory, &self.translate)?;
        let publish_conversation = conversation.clone();
        let publish_id = session_id.clone();
        let publish_directory = directory.clone();
        let publish_translate = self.translate.clone();
        let subscription = directory.subscribe(Rc::new(move || {
            let _ = publish_block(
                &publish_conversation,
                &publish_id,
                &publish_directory,
                &publish_translate,
            );
        }));
        let subscription = Rc::new(RefCell::new(Some(subscription)));
        let cleanup_conversation = conversation;
        let cleanup_id = session_id.clone();
        let setup = Closure::wrap(Box::new(move || -> JsValue {
            let subscription = subscription.clone();
            let conversation = cleanup_conversation.clone();
            let id = cleanup_id.clone();
            Closure::wrap(Box::new(move || {
                if let Some(mut subscription) = subscription.borrow_mut().take() {
                    subscription.dispose();
                }
                let _ = clear_block(&conversation, &id);
            }) as Box<dyn FnMut()>)
            .into_js_value()
        }) as Box<dyn FnMut() -> JsValue>);
        call_method(
            scope,
            "effect",
            &[
                setup.into_js_value(),
                JsValue::from_str("ui-model-selection: composer block"),
            ],
        )?;
        Ok(())
    }

    fn own_directory(
        &self,
        scope: &JsValue,
        session_id: &SessionId,
        directory: &Rc<ModelDirectory>,
    ) -> Result<(), JsValue> {
        let directories = self.directories.clone();
        let id = session_id.clone();
        let directory = directory.clone();
        let setup = Closure::wrap(Box::new(move || -> JsValue {
            let directories = directories.clone();
            let id = id.clone();
            let directory = directory.clone();
            Closure::wrap(Box::new(move || {
                directory.dispose();
                directories.borrow_mut().remove(&id);
            }) as Box<dyn FnMut()>)
            .into_js_value()
        }) as Box<dyn FnMut() -> JsValue>);
        call_method(
            scope,
            "effect",
            &[
                setup.into_js_value(),
                JsValue::from_str("ui-model-selection: session directory"),
            ],
        )?;
        Ok(())
    }
}

fn publish_block(
    conversation: &JsValue,
    session_id: &SessionId,
    directory: &ModelDirectory,
    translate: &Function,
) -> Result<(), JsValue> {
    let blocks = required(conversation, "blocks", "conversation")?;
    let block = if directory.snapshot().routable == Some(false) {
        object(&[("reason", translated(translate, "blocked.composer")?)])?.into()
    } else {
        JsValue::UNDEFINED
    };
    call_method(
        &blocks,
        "set",
        &[JsValue::from_str(session_id.as_str()), block],
    )?;
    Ok(())
}

fn clear_block(conversation: &JsValue, session_id: &SessionId) -> Result<(), JsValue> {
    let blocks = required(conversation, "blocks", "conversation")?;
    call_method(
        &blocks,
        "set",
        &[JsValue::from_str(session_id.as_str()), JsValue::UNDEFINED],
    )?;
    Ok(())
}
