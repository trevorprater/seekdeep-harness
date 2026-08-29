//! Compiled session-addressed input facade registry and send choreography.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{BrowserSessionInputShell, queue_read_face_of_browser};

#[derive(Clone)]
struct ShellRecord {
    face: JsValue,
}

struct InputHubInner {
    root_ctx: JsValue,
    translate: Function,
    shells: RefCell<BTreeMap<SessionId, ShellRecord>>,
}

/// Session-addressed browser input registry and delivery owner.
#[wasm_bindgen(js_name = InputHub)]
pub struct BrowserInputHub {
    inner: Rc<InputHubInner>,
}

#[wasm_bindgen(js_class = InputHub)]
#[allow(clippy::needless_pass_by_value)] // JavaScript class methods own their ABI arguments.
impl BrowserInputHub {
    /// Creates one root-scoped input hub.
    #[wasm_bindgen(constructor)]
    pub fn new(root_ctx: JsValue, translate: Function) -> Self {
        Self {
            inner: Rc::new(InputHubInner {
                root_ctx,
                translate,
                shells: RefCell::new(BTreeMap::new()),
            }),
        }
    }

    /// Resolves the resident shell addressed by one session scope.
    ///
    /// # Errors
    ///
    /// Returns when the context is not session-scoped or its binding is unavailable.
    #[wasm_bindgen(js_name = for)]
    pub fn for_context(&self, actx: JsValue) -> Result<JsValue, JsValue> {
        self.inner.for_context(&actx)
    }

    /// Materializes or returns the resident shell for one binding.
    ///
    /// # Errors
    ///
    /// Returns for malformed bindings, shell construction, or lifecycle-wiring failures.
    #[wasm_bindgen(js_name = shellFor)]
    pub fn shell_for(&self, binding: JsValue) -> Result<JsValue, JsValue> {
        self.inner.shell_for(&binding)
    }

    /// Resolves one shell by session id.
    ///
    /// # Errors
    ///
    /// Returns when the Sessions service or addressed binding is unavailable.
    pub fn shell(&self, session_id: String) -> Result<JsValue, JsValue> {
        self.inner.shell(&SessionId::new(session_id))
    }

    /// Returns the package-private composer keyboard face.
    ///
    /// # Errors
    ///
    /// Returns when the addressed shell cannot be resolved.
    pub fn keyboard(&self, session_id: String) -> Result<JsValue, JsValue> {
        self.inner.shell(&SessionId::new(session_id))
    }

    /// Resolves the optional input-trigger controller for one session.
    ///
    /// # Errors
    ///
    /// Returns for Sessions or controller-resolution failures.
    #[wasm_bindgen(js_name = inputTriggers)]
    pub fn input_triggers(&self, session_id: String) -> Result<JsValue, JsValue> {
        self.inner.input_triggers(&SessionId::new(session_id))
    }
}

impl InputHubInner {
    fn for_context(self: &Rc<Self>, actx: &JsValue) -> Result<JsValue, JsValue> {
        let sessions = self.sessions()?;
        let id = call_method(&sessions, "scopeOf", std::slice::from_ref(actx))?;
        let Some(id) = id.as_string() else {
            return Err(
                js_sys::Error::new("conversation.input.for requires a session scope").into(),
            );
        };
        self.shell(&SessionId::new(id))
    }

    fn shell(self: &Rc<Self>, session_id: &SessionId) -> Result<JsValue, JsValue> {
        if let Some(record) = self.shells.borrow().get(session_id) {
            return Ok(record.face.clone());
        }
        let binding = call_method(
            &self.sessions()?,
            "binding",
            &[JsValue::from_str(session_id.as_str())],
        )?;
        if binding.is_null() || binding.is_undefined() {
            return Err(js_sys::Error::new(&format!(
                "conversation.input: session \"{}\" resolved no binding",
                session_id.as_str()
            ))
            .into());
        }
        self.shell_for(&binding)
    }

    fn shell_for(self: &Rc<Self>, binding: &JsValue) -> Result<JsValue, JsValue> {
        let id = SessionId::new(required_string(binding, "sessionId", "Session binding")?);
        if let Some(record) = self.shells.borrow().get(&id) {
            return Ok(record.face.clone());
        }
        let session = required(binding, "session", "Session binding")?;
        let actx = required(binding, "ctx", "Session binding")?;
        let shell_slot = Rc::new(RefCell::new(None::<JsValue>));
        let deps = self.shell_dependencies(&session, &actx, &shell_slot)?;
        let shell: JsValue = BrowserSessionInputShell::new(deps)?.into();
        *shell_slot.borrow_mut() = Some(shell.clone());
        self.shells.borrow_mut().insert(
            id.clone(),
            ShellRecord {
                face: shell.clone(),
            },
        );
        self.own_shell_scope(&id, &actx, &shell)?;
        Ok(shell)
    }

    fn shell_dependencies(
        self: &Rc<Self>,
        session: &JsValue,
        actx: &JsValue,
        shell_slot: &Rc<RefCell<Option<JsValue>>>,
    ) -> Result<JsValue, JsValue> {
        let deps = Object::new();
        set(&deps, "actx", actx)?;
        set(
            &deps,
            "queue",
            &queue_read_face_of_browser(session.clone())?,
        )?;

        let hub = Rc::downgrade(self);
        let controller_actx = actx.clone();
        let input_triggers = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let Some(hub) = hub.upgrade() else {
                return Ok(JsValue::UNDEFINED);
            };
            hub.controller(&controller_actx)
        })
            as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        set(&deps, "inputTriggers", &input_triggers.into_js_value())?;

        let hub = Rc::downgrade(self);
        let popup_actx = actx.clone();
        let popup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let Some(hub) = hub.upgrade() else {
                return Ok(JsValue::UNDEFINED);
            };
            hub.popup(&popup_actx)
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        set(&deps, "popup", &popup.into_js_value())?;

        let hub = Rc::downgrade(self);
        let sink_session = session.clone();
        let default_sink = Closure::wrap(Box::new(
            move |text: String, image_ids: JsValue, mode: String| -> Result<(), JsValue> {
                let Some(hub) = hub.upgrade() else {
                    return Ok(());
                };
                hub.sink(&sink_session, &text, &image_ids, &mode)
            },
        )
            as Box<dyn FnMut(String, JsValue, String) -> Result<(), JsValue>>);
        set(&deps, "defaultSink", &default_sink.into_js_value())?;

        let hub = Rc::downgrade(self);
        let steer_session = session.clone();
        let steer_shell = shell_slot.clone();
        let steer_queue = Closure::wrap(Box::new(move || {
            let Some(hub) = hub.upgrade() else {
                return;
            };
            let Some(shell) = steer_shell.borrow().clone() else {
                return;
            };
            if let Err(error) = hub.steer_queue(&steer_session, &shell) {
                report_unhandled(&error);
            }
        }) as Box<dyn FnMut()>);
        set(&deps, "steerQueue", &steer_queue.into_js_value())?;
        Ok(deps.into())
    }

    fn own_shell_scope(
        self: &Rc<Self>,
        id: &SessionId,
        actx: &JsValue,
        shell: &JsValue,
    ) -> Result<(), JsValue> {
        let weak = Rc::downgrade(self);
        let cleanup_id = id.clone();
        let cleanup_shell = shell.clone();
        let cleanup_ctx = self.root_ctx.clone();
        let setup_actx = actx.clone();
        let setup_shell = shell.clone();
        let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let disposers = vec![
                own_input_listener(
                    &setup_actx,
                    &setup_shell,
                    "slash/input-begin-command",
                    InputListenerKind::BeginCommand,
                )?,
                own_input_listener(
                    &setup_actx,
                    &setup_shell,
                    "slash/input-insert-reference",
                    InputListenerKind::InsertReference,
                )?,
                own_input_listener(
                    &setup_actx,
                    &setup_shell,
                    "slash/input-consume-token",
                    InputListenerKind::ConsumeToken,
                )?,
                own_input_listener(
                    &setup_actx,
                    &setup_shell,
                    "slash/input-insert-text",
                    InputListenerKind::InsertText,
                )?,
            ];
            let weak = weak.clone();
            let id = cleanup_id.clone();
            let shell = cleanup_shell.clone();
            let root_ctx = cleanup_ctx.clone();
            Ok(Closure::wrap(Box::new(move || -> Result<(), JsValue> {
                for disposer in &disposers {
                    disposer.call0(&JsValue::UNDEFINED)?;
                }
                let snapshot = Reflect::get(&shell, &JsValue::from_str("snapshot"))?;
                let images = required(&snapshot, "imageIds", "InputState")?.dyn_into::<Array>()?;
                call_method(&shell, "dispose", &[])?;
                if let Some(hub) = weak.upgrade() {
                    hub.shells.borrow_mut().remove(&id);
                }
                let conversation =
                    call_method(&root_ctx, "get", &[JsValue::from_str("conversation")])?;
                if !conversation.is_null() && !conversation.is_undefined() {
                    for image_id in images.iter() {
                        call_method(
                            &conversation,
                            "releaseDraftImage",
                            std::slice::from_ref(&image_id),
                        )?;
                    }
                }
                Ok(())
            }) as Box<dyn FnMut() -> Result<(), JsValue>>)
            .into_js_value())
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        call_method(
            actx,
            "effect",
            &[
                setup.into_js_value(),
                JsValue::from_str("conversation.input: session shell"),
            ],
        )?;
        Ok(())
    }

    fn input_triggers(self: &Rc<Self>, id: &SessionId) -> Result<JsValue, JsValue> {
        let actx = call_method(
            &self.sessions()?,
            "scope",
            &[JsValue::from_str(id.as_str())],
        )?;
        if actx.is_null() || actx.is_undefined() {
            return Ok(JsValue::UNDEFINED);
        }
        self.controller(&actx)
    }

    fn controller(&self, actx: &JsValue) -> Result<JsValue, JsValue> {
        let Some(service) = self.root_service("inputTriggers")? else {
            return Ok(JsValue::UNDEFINED);
        };
        call_method(&service, "sessionOf", std::slice::from_ref(actx))
    }

    fn popup(&self, actx: &JsValue) -> Result<JsValue, JsValue> {
        let Some(service) = self.root_service("commandUi")? else {
            return Ok(JsValue::UNDEFINED);
        };
        call_method(&service, "popupFor", std::slice::from_ref(actx))
    }

    fn sink(
        self: &Rc<Self>,
        session: &JsValue,
        text: &str,
        image_ids: &JsValue,
        mode: &str,
    ) -> Result<(), JsValue> {
        let images = image_ids.clone().dyn_into::<Array>()?;
        if text.is_empty() && images.length() == 0 {
            return Ok(());
        }
        let id = SessionId::new(required_string(session, "sessionId", "Session face")?);
        let submitted_shell = self
            .shells
            .borrow()
            .get(&id)
            .map(|record| record.face.clone());
        if let Some(shell) = submitted_shell.as_ref() {
            call_method(shell, "commitSend", std::slice::from_ref(image_ids))?;
        }
        let conversation = self.conversation()?;
        let returned = call_method(
            &conversation,
            "sendSession",
            &[
                session.clone(),
                JsValue::from_str(text),
                image_ids.clone(),
                JsValue::from_str(mode),
            ],
        )?;
        let promise = returned.dyn_into::<Promise>()?;
        let hub = self.clone();
        let rollback_session = session.clone();
        let rollback_text = text.to_owned();
        let rollback_images = image_ids.clone();
        let rollback = Closure::wrap(Box::new(move |_error: JsValue| {
            if let Err(error) = hub.rollback_send(
                &rollback_session,
                &rollback_text,
                &rollback_images,
                submitted_shell.as_ref(),
            ) {
                wasm_bindgen::throw_val(error);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let _ = promise.catch(&rollback);
        drop(rollback.into_js_value());
        Ok(())
    }

    fn rollback_send(
        &self,
        session: &JsValue,
        text: &str,
        image_ids: &JsValue,
        submitted_shell: Option<&JsValue>,
    ) -> Result<(), JsValue> {
        let id = SessionId::new(required_string(session, "sessionId", "Session face")?);
        let current = self
            .shells
            .borrow()
            .get(&id)
            .map(|record| record.face.clone());
        if same_optional_face(current.as_ref(), submitted_shell) {
            if let Some(shell) = submitted_shell {
                call_method(shell, "restoreImages", std::slice::from_ref(image_ids))?;
                let snapshot = Reflect::get(shell, &JsValue::from_str("snapshot"))?;
                if required_string(&snapshot, "draft", "InputState")?.is_empty() {
                    call_method(shell, "setDraft", &[JsValue::from_str(text)])?;
                }
            }
            return Ok(());
        }
        let Some(conversation) = self.root_service("conversation")? else {
            return Ok(());
        };
        let images = image_ids.clone().dyn_into::<Array>()?;
        for image_id in images.iter() {
            call_method(
                &conversation,
                "releaseDraftImage",
                std::slice::from_ref(&image_id),
            )?;
        }
        Ok(())
    }

    fn steer_queue(self: &Rc<Self>, session: &JsValue, shell: &JsValue) -> Result<(), JsValue> {
        let snapshot = call_method(session, "getSnapshot", &[])?;
        let queue = required(&snapshot, "queue", "Session snapshot")?.dyn_into::<Array>()?;
        let queued = Rc::new(
            queue
                .iter()
                .filter(|item| {
                    Reflect::get(item, &JsValue::from_str("placement"))
                        .ok()
                        .and_then(|placement| placement.as_string())
                        .as_deref()
                        == Some("queued")
                })
                .collect::<Vec<_>>(),
        );
        self.steer_rows(session, shell, &queued, 0)
    }

    fn steer_rows(
        self: &Rc<Self>,
        session: &JsValue,
        shell: &JsValue,
        queued: &Rc<Vec<JsValue>>,
        index: usize,
    ) -> Result<(), JsValue> {
        let Some(item) = queued.get(index) else {
            return Ok(());
        };
        let id = required(item, "id", "queued message")?;
        let update = object(&[("kind", JsValue::from_str("steer"))])?;
        let returned = call_method(session, "updateQueue", &[id, update.into()])?;
        let promise = returned.dyn_into::<Promise>()?;
        let hub = self.clone();
        let next_session = session.clone();
        let next_shell = shell.clone();
        let next_queue = queued.clone();
        let fulfilled = Closure::wrap(Box::new(move |result: JsValue| {
            if let Err(error) =
                hub.handle_steer_result(&next_session, &next_shell, &next_queue, index, &result)
            {
                wasm_bindgen::throw_val(error);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let _ = promise.then(&fulfilled);
        drop(fulfilled.into_js_value());
        Ok(())
    }

    fn handle_steer_result(
        self: &Rc<Self>,
        session: &JsValue,
        shell: &JsValue,
        queued: &Rc<Vec<JsValue>>,
        index: usize,
        result: &JsValue,
    ) -> Result<(), JsValue> {
        if Reflect::get(result, &JsValue::from_str("ok"))?.as_bool() == Some(true) {
            return self.steer_rows(session, shell, queued, index.saturating_add(1));
        }
        let error = required(result, "error", "queue update result")?;
        let code = required_string(&error, "code", "queue update error")?;
        if matches!(code.as_str(), "steer-unavailable" | "queue-item-not-found") {
            return Ok(());
        }
        let text = self
            .translate
            .call1(&JsValue::UNDEFINED, &JsValue::from_str("queue.steerFailed"))?
            .as_string()
            .ok_or_else(|| {
                js_sys::TypeError::new("queue.steerFailed must translate to a string")
            })?;
        call_method(
            shell,
            "notify",
            &[JsValue::from_str("error"), JsValue::from_str(&text)],
        )?;
        Ok(())
    }

    fn sessions(&self) -> Result<JsValue, JsValue> {
        self.root_service("sessions")?.ok_or_else(|| {
            js_sys::Error::new("conversation.input: sessions service unavailable").into()
        })
    }

    fn conversation(&self) -> Result<JsValue, JsValue> {
        self.root_service("conversation")?.ok_or_else(|| {
            js_sys::Error::new("conversation.input: conversation service unavailable").into()
        })
    }

    fn root_service(&self, name: &str) -> Result<Option<JsValue>, JsValue> {
        let service = call_method(&self.root_ctx, "get", &[JsValue::from_str(name)])?;
        Ok((!service.is_null() && !service.is_undefined()).then_some(service))
    }
}

#[derive(Clone, Copy)]
enum InputListenerKind {
    BeginCommand,
    InsertReference,
    ConsumeToken,
    InsertText,
}

fn own_input_listener(
    actx: &JsValue,
    shell: &JsValue,
    event: &str,
    kind: InputListenerKind,
) -> Result<Function, JsValue> {
    let shell = shell.clone();
    let listener = Closure::wrap(
        Box::new(move |request: JsValue| -> Result<JsValue, JsValue> {
            let (method, arguments) = match kind {
                InputListenerKind::BeginCommand => (
                    "beginCommand",
                    vec![
                        required(&request, "claim", "begin-command request")?,
                        required(&request, "span", "begin-command request")?,
                    ],
                ),
                InputListenerKind::InsertReference => (
                    "insertReference",
                    vec![
                        required(&request, "reference", "insert-reference request")?,
                        required(&request, "span", "insert-reference request")?,
                    ],
                ),
                InputListenerKind::ConsumeToken => (
                    "consumeToken",
                    vec![required(&request, "guard", "consume-token request")?],
                ),
                InputListenerKind::InsertText => (
                    "insertText",
                    vec![
                        required(&request, "text", "insert-text request")?,
                        required(&request, "span", "insert-text request")?,
                    ],
                ),
            };
            Ok(
                if call_method(&shell, method, &arguments)?.as_bool() == Some(true) {
                    JsValue::TRUE
                } else {
                    JsValue::UNDEFINED
                },
            )
        }) as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
    );
    call_method(
        actx,
        "on",
        &[JsValue::from_str(event), listener.into_js_value()],
    )?
    .dyn_into()
}

fn same_optional_face(left: Option<&JsValue>, right: Option<&JsValue>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Object::is(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn report_unhandled(error: &JsValue) {
    let promise = Promise::reject(error);
    drop(promise);
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        set(&object, key, value)?;
    }
    Ok(object)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(key), value).map(|_| ())
}

fn call_method(value: &JsValue, key: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required(value, key, "object")?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    function.apply(value, &arguments)
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}
