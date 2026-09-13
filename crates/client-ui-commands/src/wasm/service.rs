//! Browser `CommandUiRuntime`: source, directory, contribution registry, execution, and popups.

use std::{cell::RefCell, rc::Rc};

use indexmap::IndexMap;
use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_commands_contract::CommandDescriptor;
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

use super::{
    WasmPopupSelectController,
    browser_directory::{BrowserDirectoryTransport, BrowserSpawner},
    call_method, js_error_string, object, optional, required, required_function, required_string,
    set, to_js,
};
use crate::{
    CommandDirectory, PopupSelectController, PopupSelectDeps, PopupTokenSegment, fuzzy_candidates,
    submitted_command_name,
};

struct PopupRecord {
    inner: Rc<PopupSelectController>,
    face: JsValue,
}

struct RuntimeState {
    contributions: IndexMap<String, JsValue>,
    decorations: IndexMap<String, JsValue>,
    popups: IndexMap<SessionId, PopupRecord>,
    focus_hooks: IndexMap<SessionId, Function>,
}

/// Root browser command service.
pub(crate) struct BrowserCommandUiRuntime {
    ctx: JsValue,
    sessions: JsValue,
    directory: Rc<CommandDirectory>,
    state: RefCell<RuntimeState>,
}

impl BrowserCommandUiRuntime {
    pub(crate) fn new(ctx: &JsValue) -> Result<Rc<Self>, JsValue> {
        let sessions = required(ctx, "sessions", "Client Context")?;
        let remote = required(ctx, "remote", "Client Context")?;
        let commands = required(&remote, "commands", "remote")?;
        let input_triggers = required(ctx, "inputTriggers", "Client Context")?;
        let runtime = Rc::new(Self {
            ctx: ctx.clone(),
            sessions: sessions.clone(),
            directory: CommandDirectory::new(
                BrowserDirectoryTransport::for_runtime(commands, sessions),
                Rc::new(BrowserSpawner),
            ),
            state: RefCell::new(RuntimeState {
                contributions: IndexMap::new(),
                decorations: IndexMap::new(),
                popups: IndexMap::new(),
                focus_hooks: IndexMap::new(),
            }),
        });
        runtime.install_source(&input_triggers)?;
        runtime.install_events(&remote)?;
        Ok(runtime)
    }

    pub(crate) fn face(runtime: &Rc<Self>) -> JsValue {
        WasmCommandUiRuntime::from_inner(runtime.clone()).into()
    }

    pub(crate) fn popup_face(self: &Rc<Self>, actx: &JsValue) -> Result<JsValue, JsValue> {
        let id = self.scope_id(actx)?;
        if let Some(record) = self.state.borrow().popups.get(&id) {
            return Ok(record.face.clone());
        }
        let deps = Rc::new(RuntimePopupDeps {
            actx: actx.clone(),
            session_id: id.clone(),
            runtime: Rc::downgrade(self),
        });
        let popup = PopupSelectController::new(
            deps,
            Rc::new(super::browser_popup::BrowserAbortFactory),
            Rc::new(super::browser_popup::BrowserPopupSpawner),
        );
        let face: JsValue = WasmPopupSelectController::from_inner(popup.clone())?.into();
        self.state.borrow_mut().popups.insert(
            id.clone(),
            PopupRecord {
                inner: popup.clone(),
                face: face.clone(),
            },
        );
        let runtime = Rc::downgrade(self);
        let dispose_popup = popup;
        let setup = Closure::wrap(Box::new(move || -> JsValue {
            let runtime = runtime.clone();
            let id = id.clone();
            let popup = dispose_popup.clone();
            Closure::wrap(Box::new(move || {
                popup.dispose();
                if let Some(runtime) = runtime.upgrade() {
                    let mut state = runtime.state.borrow_mut();
                    state.popups.shift_remove(&id);
                    state.focus_hooks.shift_remove(&id);
                }
            }) as Box<dyn FnMut()>)
            .into_js_value()
        }) as Box<dyn FnMut() -> JsValue>);
        call_method(
            actx,
            "effect",
            &[
                setup.into_js_value(),
                JsValue::from_str("command: session popup"),
            ],
        )?;
        Ok(face)
    }

    pub(crate) fn dispose_all(&self) {
        let popups = std::mem::take(&mut self.state.borrow_mut().popups);
        for record in popups.into_values() {
            record.inner.dispose();
        }
        self.state.borrow_mut().focus_hooks.clear();
    }

    fn install_source(self: &Rc<Self>, input_triggers: &JsValue) -> Result<(), JsValue> {
        let source = Object::new();
        set(&source, "trigger", &JsValue::from_str("/"))?;
        set(&source, "name", &JsValue::from_str("command"))?;
        let candidate_runtime = self.clone();
        let candidates = Closure::wrap(Box::new(
            move |session: JsValue, request: JsValue| -> Promise {
                candidate_runtime.candidates(session, request)
            },
        ) as Box<dyn FnMut(JsValue, JsValue) -> Promise>);
        set(&source, "candidates", &candidates.into_js_value())?;
        let pick_runtime = self.clone();
        let pick = Closure::wrap(
            Box::new(move |input: JsValue| pick_runtime.dispatch(&input))
                as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>,
        );
        set(&source, "onPick", &pick.into_js_value())?;
        let space_runtime = self.clone();
        let space = Closure::wrap(Box::new(move |session: JsValue, token: String| {
            space_runtime.match_space(&session, &token)
        })
            as Box<dyn FnMut(JsValue, String) -> Result<JsValue, JsValue>>);
        set(&source, "matchSpace", &space.into_js_value())?;
        let enter_runtime = self.clone();
        let enter = Closure::wrap(Box::new(
            move |session: JsValue, line: String, signal: JsValue| {
                enter_runtime.match_enter(session, line, signal)
            },
        )
            as Box<dyn FnMut(JsValue, String, JsValue) -> Promise>);
        set(&source, "matchEnter", &enter.into_js_value())?;
        let warm_directory = self.directory.clone();
        let warm = Closure::wrap(Box::new(move |session: JsValue| -> Result<(), JsValue> {
            let id = required_string(&session, "sessionId", "Client Session Context")?;
            warm_directory.warm(SessionId::new(id));
            Ok(())
        }) as Box<dyn FnMut(JsValue) -> Result<(), JsValue>>);
        set(&source, "warm", &warm.into_js_value())?;
        let triggers = input_triggers.clone();
        let source: JsValue = source.into();
        let setup = Closure::wrap(Box::new(move || {
            call_method(&triggers, "registerSource", std::slice::from_ref(&source))
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        call_method(
            &self.ctx,
            "effect",
            &[
                setup.into_js_value(),
                JsValue::from_str("command: slash source"),
            ],
        )?;
        Ok(())
    }

    fn install_events(self: &Rc<Self>, remote: &JsValue) -> Result<(), JsValue> {
        let invalidate = self.directory.clone();
        let changed =
            Closure::wrap(Box::new(move || invalidate.invalidate_all()) as Box<dyn FnMut()>);
        call_method(
            remote,
            "$on",
            &[
                JsValue::from_str("commands/change"),
                changed.into_js_value(),
            ],
        )?;
        let preset = self.directory.clone();
        let preset_listener = Closure::wrap(Box::new(move |session_id: String, _preset: JsValue| {
            let future = preset.refresh(SessionId::new(session_id));
            spawn_local(future);
        }) as Box<dyn FnMut(String, JsValue)>);
        call_method(
            remote,
            "$on",
            &[
                JsValue::from_str("agent-preset/selected"),
                preset_listener.into_js_value(),
            ],
        )?;
        let reset = self.directory.clone();
        let reset_listener =
            Closure::wrap(Box::new(move || reset.reset_connected()) as Box<dyn FnMut()>);
        call_method(
            &self.ctx,
            "on",
            &[
                JsValue::from_str("connection/reset"),
                reset_listener.into_js_value(),
            ],
        )?;
        Ok(())
    }

    fn register(self: &Rc<Self>, map: RegistryKind, value: &JsValue) -> Result<Function, JsValue> {
        let name = required_string(value, "name", "command registration")?;
        let runtime = self.clone();
        let setup_value = value.clone();
        let setup_name = name.clone();
        let setup = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
            let mut state = runtime.state.borrow_mut();
            let registry = map.registry_mut(&mut state);
            if registry.contains_key(&setup_name) {
                return Err(js_sys::Error::new(&format!(
                    "ui-commands: duplicate {} for /{}",
                    map.label(),
                    setup_name
                ))
                .into());
            }
            registry.insert(setup_name.clone(), setup_value.clone());
            let runtime = Rc::downgrade(&runtime);
            let name = setup_name.clone();
            Ok(Closure::wrap(Box::new(move || {
                if let Some(runtime) = runtime.upgrade() {
                    map.registry_mut(&mut runtime.state.borrow_mut())
                        .shift_remove(&name);
                }
            }) as Box<dyn FnMut()>)
            .into_js_value())
        }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
        let disposer = call_method(
            &self.ctx,
            "effect",
            &[setup.into_js_value(), JsValue::from_str(map.effect_label())],
        )?;
        Ok(Closure::wrap(Box::new(move || {
            if let Ok(function) = disposer.clone().dyn_into::<Function>() {
                let _ = function.call0(&JsValue::UNDEFINED);
            }
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into())
    }

    fn bind_focus_rc(self: &Rc<Self>, session_id: String, focus: Function) -> Function {
        let id = SessionId::new(session_id);
        self.state
            .borrow_mut()
            .focus_hooks
            .insert(id.clone(), focus.clone());
        let runtime = Rc::downgrade(self);
        Closure::wrap(Box::new(move || {
            if let Some(runtime) = runtime.upgrade() {
                let mut state = runtime.state.borrow_mut();
                if state
                    .focus_hooks
                    .get(&id)
                    .is_some_and(|current| Object::is(current, &focus))
                {
                    state.focus_hooks.shift_remove(&id);
                }
            }
        }) as Box<dyn FnMut()>)
        .into_js_value()
        .unchecked_into()
    }

    fn candidates(self: &Rc<Self>, session: JsValue, request: JsValue) -> Promise {
        let runtime = self.clone();
        future_to_promise(async move {
            let session_id = SessionId::new(required_string(
                &session,
                "sessionId",
                "Client Session Context",
            )?);
            let signal = required(&request, "signal", "candidate request")?;
            let commands = runtime
                .directory
                .ensure_ready(
                    session_id,
                    super::browser_directory::BrowserDirectoryAbort::new(signal),
                )
                .await
                .map_err(|message| js_sys::Error::new(&message))?;
            let position = required_string(&request, "position", "candidate request")?;
            let query = required_string(&request, "query", "candidate request")?;
            let mut rows = commands
                .iter()
                .filter(|command| position == "leading" || command.input.is_none())
                .map(
                    |command| seekdeep_client_ui_input_trigger::InputTriggerCandidate {
                        name: command.name.clone(),
                        description: Some(command.description.clone()),
                        icon: None,
                        hint: command.input.as_ref().map(|input| input.hint.clone()),
                    },
                )
                .collect::<Vec<_>>();
            let mut seen = commands
                .iter()
                .map(|command| command.name.clone())
                .collect::<std::collections::BTreeSet<_>>();
            for (name, contribution) in runtime.state.borrow().contributions.clone() {
                if !available(&contribution, &session)? {
                    continue;
                }
                if !seen.insert(name.clone()) {
                    return Err(js_sys::Error::new(&format!(
                        "ui-commands: contribution /{name} collides with a host command"
                    ))
                    .into());
                }
                rows.push(seekdeep_client_ui_input_trigger::InputTriggerCandidate {
                    name,
                    description: Some(required_string(
                        &contribution,
                        "description",
                        "command contribution",
                    )?),
                    icon: None,
                    hint: None,
                });
            }
            let ranked = fuzzy_candidates(&rows, &query)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            serde_wasm_bindgen::to_value(&ranked)
                .map_err(|error| js_sys::Error::new(&error.to_string()).into())
        })
    }

    fn dispatch(self: &Rc<Self>, pick: &JsValue) -> Result<JsValue, JsValue> {
        let candidate = required(pick, "candidate", "trigger pick")?;
        let name = required_string(&candidate, "name", "trigger candidate")?;
        let session = required(pick, "session", "trigger pick")?;
        let session_id = SessionId::new(required_string(
            &session,
            "sessionId",
            "Client Session Context",
        )?);
        if let Some(contribution) = self.state.borrow().contributions.get(&name).cloned()
            && available(&contribution, &session)?
        {
            self.open_popup(
                &name,
                &required(&contribution, "ui", "command contribution")?,
                &session,
                &menu_segment(pick)?,
            )?;
            return Ok(JsValue::from_str("handled"));
        }
        let Some(descriptor) = self.directory.resolve(&session_id, &name) else {
            return Ok(JsValue::UNDEFINED);
        };
        if let Some(decoration) = self.state.borrow().decorations.get(&name).cloned()
            && available(&decoration, &session)?
        {
            self.open_popup(
                &name,
                &required(&decoration, "ui", "command decoration")?,
                &session,
                &menu_segment(pick)?,
            )?;
            return Ok(JsValue::from_str("handled"));
        }
        if descriptor.input.is_some() {
            return self.claim(&descriptor, &session);
        }
        self.consume_via(&session_id, &menu_segment(pick)?)?;
        self.run_detached(descriptor, session, format!("/{name}"));
        Ok(JsValue::from_str("handled"))
    }

    fn match_space(&self, session: &JsValue, token: &str) -> Result<JsValue, JsValue> {
        let Some(name) = token.strip_prefix('/') else {
            return Ok(JsValue::UNDEFINED);
        };
        if self.state.borrow().contributions.contains_key(name) {
            return Ok(JsValue::UNDEFINED);
        }
        let id = SessionId::new(required_string(
            session,
            "sessionId",
            "Client Session Context",
        )?);
        let Some(descriptor) = self.directory.resolve(&id, name) else {
            return Ok(JsValue::UNDEFINED);
        };
        if descriptor.input.is_none() {
            return Ok(JsValue::UNDEFINED);
        }
        self.claim(&descriptor, session)
    }

    fn match_enter(self: &Rc<Self>, session: JsValue, line: String, signal: JsValue) -> Promise {
        let runtime = self.clone();
        future_to_promise(async move {
            let trimmed = line.trim();
            let Some(rest) = trimmed.strip_prefix('/') else {
                return Ok(JsValue::UNDEFINED);
            };
            let mut parts = rest.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or_default();
            let bare = parts.next().is_none();
            if name.is_empty() {
                return Ok(JsValue::UNDEFINED);
            }
            if let Some(contribution) = runtime.state.borrow().contributions.get(name).cloned()
                && available(&contribution, &session)?
            {
                if !bare {
                    return Ok(JsValue::UNDEFINED);
                }
                runtime.open_popup(
                    name,
                    &required(&contribution, "ui", "command contribution")?,
                    &session,
                    &PopupTokenSegment::Enter {
                        token: format!("/{name}"),
                    },
                )?;
                return Ok(JsValue::from_str("handled"));
            }
            let id = SessionId::new(required_string(
                &session,
                "sessionId",
                "Client Session Context",
            )?);
            runtime
                .directory
                .ensure_ready(
                    id.clone(),
                    super::browser_directory::BrowserDirectoryAbort::new(signal),
                )
                .await
                .map_err(|message| js_sys::Error::new(&message))?;
            let Some(descriptor) = runtime.directory.resolve(&id, name) else {
                return Ok(JsValue::UNDEFINED);
            };
            if bare
                && let Some(decoration) = runtime.state.borrow().decorations.get(name).cloned()
                && available(&decoration, &session)?
            {
                runtime.open_popup(
                    name,
                    &required(&decoration, "ui", "command decoration")?,
                    &session,
                    &PopupTokenSegment::Enter {
                        token: format!("/{name}"),
                    },
                )?;
                return Ok(JsValue::from_str("handled"));
            }
            if descriptor.input.is_some() {
                return runtime.claim(&descriptor, &session);
            }
            if !bare {
                return Ok(JsValue::UNDEFINED);
            }
            runtime.consume_via(
                &id,
                &PopupTokenSegment::Enter {
                    token: format!("/{name}"),
                },
            )?;
            runtime.run_detached(descriptor, session, trimmed.to_owned());
            Ok(JsValue::from_str("handled"))
        })
    }

    fn claim(&self, descriptor: &CommandDescriptor, session: &JsValue) -> Result<JsValue, JsValue> {
        let token = format!("/{} ", descriptor.name);
        let claim = Object::new();
        set(&claim, "token", &JsValue::from_str(&token))?;
        if let Some(input) = &descriptor.input {
            set(&claim, "hint", &JsValue::from_str(&input.hint))?;
        }
        let execute_runtime = self.ctx.clone();
        let execute_session = session.clone();
        let submit_token = token;
        let submit = Closure::wrap(Box::new(move |args: String, _actx: JsValue| -> Promise {
            execute_command(
                &execute_runtime,
                execute_session.clone(),
                format!("{submit_token}{args}"),
            )
        }) as Box<dyn FnMut(String, JsValue) -> Promise>);
        set(&claim, "submit", &submit.into_js_value())?;
        object(&[("claim", claim.into())]).map(Into::into)
    }

    fn open_popup(
        self: &Rc<Self>,
        name: &str,
        spec: &JsValue,
        session: &JsValue,
        segment: &PopupTokenSegment,
    ) -> Result<(), JsValue> {
        let id = required_string(session, "sessionId", "Client Session Context")?;
        let actx = call_method(&self.sessions, "scope", &[JsValue::from_str(&id)])?;
        if actx.is_null() || actx.is_undefined() {
            return Ok(());
        }
        let face = self.popup_face(&actx)?;
        call_method(
            &face,
            "open",
            &[
                JsValue::from_str(name),
                spec.clone(),
                session.clone(),
                to_js(&segment)?,
            ],
        )?;
        Ok(())
    }

    fn consume_via(&self, id: &SessionId, segment: &PopupTokenSegment) -> Result<(), JsValue> {
        let actx = call_method(&self.sessions, "scope", &[JsValue::from_str(id.as_str())])?;
        if actx.is_null() || actx.is_undefined() {
            return Ok(());
        }
        RuntimePopupDeps::consume_with(&actx, segment)?;
        Ok(())
    }

    fn run_detached(
        self: &Rc<Self>,
        descriptor: CommandDescriptor,
        session: JsValue,
        line: String,
    ) {
        let runtime = self.clone();
        spawn_local(async move {
            let result = JsFuture::from(execute_command(&runtime.ctx, session.clone(), line)).await;
            let message = match result {
                Ok(value)
                    if required_string(&value, "kind", "command outcome").as_deref()
                        == Ok("error") =>
                {
                    optional(&value, "text")
                        .ok()
                        .flatten()
                        .and_then(|value| value.as_string())
                        .unwrap_or_else(|| format!("/{} failed", descriptor.name))
                }
                Ok(_) => return,
                Err(error) => js_error_string(&error),
            };
            let _ = runtime.notice_for(&session, &message);
        });
    }

    fn notice_for(&self, session: &JsValue, message: &str) -> Result<(), JsValue> {
        let id = required_string(session, "sessionId", "Client Session Context")?;
        let actx = call_method(&self.sessions, "scope", &[JsValue::from_str(&id)])?;
        if actx.is_null() || actx.is_undefined() {
            return Ok(());
        }
        let conversation = call_method(&actx, "get", &[JsValue::from_str("conversation")])?;
        if conversation.is_null() || conversation.is_undefined() {
            return Ok(());
        }
        let input = required(&conversation, "input", "conversation")?;
        let face = call_method(&input, "for", std::slice::from_ref(&actx))?;
        call_method(
            &face,
            "notify",
            &[JsValue::from_str("error"), JsValue::from_str(message)],
        )?;
        Ok(())
    }

    fn scope_id(&self, actx: &JsValue) -> Result<SessionId, JsValue> {
        let id = call_method(&self.sessions, "scopeOf", std::slice::from_ref(actx))?;
        id.as_string()
            .map(SessionId::new)
            .ok_or_else(|| js_sys::Error::new("command.popupFor requires a session scope").into())
    }
}

#[derive(Clone, Copy)]
enum RegistryKind {
    Contribution,
    Decoration,
}

impl RegistryKind {
    fn registry_mut(self, state: &mut RuntimeState) -> &mut IndexMap<String, JsValue> {
        match self {
            Self::Contribution => &mut state.contributions,
            Self::Decoration => &mut state.decorations,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Contribution => "contribution",
            Self::Decoration => "decoration",
        }
    }

    const fn effect_label(self) -> &'static str {
        match self {
            Self::Contribution => "command.register()",
            Self::Decoration => "command.decorate()",
        }
    }
}

struct RuntimePopupDeps {
    actx: JsValue,
    session_id: SessionId,
    runtime: std::rc::Weak<BrowserCommandUiRuntime>,
}

impl RuntimePopupDeps {
    fn consume_with(actx: &JsValue, segment: &PopupTokenSegment) -> Result<bool, JsValue> {
        let guard = match segment {
            PopupTokenSegment::Menu { span } => {
                object(&[("kind", JsValue::from_str("span")), ("span", to_js(span)?)])?
            }
            PopupTokenSegment::Enter { token } => object(&[
                ("kind", JsValue::from_str("bare-token")),
                ("token", JsValue::from_str(token)),
            ])?,
        };
        let payload = object(&[("guard", guard.into())])?;
        Ok(call_method(
            actx,
            "bail",
            &[
                actx.clone(),
                JsValue::from_str("slash/input-consume-token"),
                payload.into(),
            ],
        )?
        .as_bool()
            == Some(true))
    }
}

impl PopupSelectDeps for RuntimePopupDeps {
    fn consume(&self, segment: &PopupTokenSegment) -> bool {
        Self::consume_with(&self.actx, segment).unwrap_or(false)
    }

    fn focus_composer(&self) {
        let focus = self.runtime.upgrade().and_then(|runtime| {
            runtime
                .state
                .borrow()
                .focus_hooks
                .get(&self.session_id)
                .cloned()
        });
        if let Some(focus) = focus {
            let _ = focus.call0(&JsValue::UNDEFINED);
        }
    }
}

/// Compiled command service face.
#[wasm_bindgen(js_name = __CommandUiRuntime)]
pub struct WasmCommandUiRuntime {
    inner: Rc<BrowserCommandUiRuntime>,
}

#[wasm_bindgen(js_class = __CommandUiRuntime)]
impl WasmCommandUiRuntime {
    /// Creates and wires the root service.
    ///
    /// # Errors
    ///
    /// Returns missing dependencies or source/event registration failures.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(ctx: JsValue) -> Result<Self, JsValue> {
        let runtime = BrowserCommandUiRuntime::new(&ctx)?;
        super::plugin::own_service(&ctx, &runtime)?;
        Ok(Self::from_inner(runtime))
    }

    /// Registers one client command contribution.
    ///
    /// # Errors
    ///
    /// Returns malformed or duplicate registration diagnostics.
    #[allow(clippy::needless_pass_by_value)]
    pub fn register(&self, value: JsValue) -> Result<Function, JsValue> {
        self.inner.register(RegistryKind::Contribution, &value)
    }

    /// Registers one Host command decoration.
    ///
    /// # Errors
    ///
    /// Returns malformed or duplicate registration diagnostics.
    #[allow(clippy::needless_pass_by_value)]
    pub fn decorate(&self, value: JsValue) -> Result<Function, JsValue> {
        self.inner.register(RegistryKind::Decoration, &value)
    }

    /// Resolves one Session popup controller.
    ///
    /// # Errors
    ///
    /// Returns unscoped context or Store-construction failures.
    #[wasm_bindgen(js_name = popupFor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn popup_for(&self, actx: JsValue) -> Result<JsValue, JsValue> {
        self.inner.popup_face(&actx)
    }

    /// Binds one composer focus callback.
    #[wasm_bindgen(js_name = bindComposerFocus)]
    pub fn bind_composer_focus(&self, session_id: String, focus: Function) -> Function {
        self.inner.bind_focus_rc(session_id, focus)
    }
}

impl WasmCommandUiRuntime {
    pub(crate) fn from_inner(inner: Rc<BrowserCommandUiRuntime>) -> Self {
        Self { inner }
    }
}

fn available(value: &JsValue, session: &JsValue) -> Result<bool, JsValue> {
    Ok(
        required_function(value, "available", "command registration")?
            .call1(value, session)?
            .is_truthy(),
    )
}

fn menu_segment(pick: &JsValue) -> Result<PopupTokenSegment, JsValue> {
    Ok(PopupTokenSegment::Menu {
        span: serde_wasm_bindgen::from_value(required(pick, "span", "trigger pick")?)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?,
    })
}

fn execute_command(ctx: &JsValue, session: JsValue, line: String) -> Promise {
    let ctx = ctx.clone();
    future_to_promise(async move {
        let remote = required(&ctx, "remote", "Client Context")?;
        let commands = required(&remote, "commands", "remote")?;
        let id = required_string(&session, "sessionId", "Client Session Context")?;
        let returned = call_method(
            &commands,
            "execute",
            &[JsValue::from_str(&id), JsValue::from_str(&line)],
        )?;
        let result = JsFuture::from(Promise::resolve(&returned)).await?;
        let ok = required(&result, "ok", "command.execute result")?
            .as_bool()
            .unwrap_or(false);
        if !ok {
            let error = required(&result, "error", "command.execute result")?;
            let code = required_string(&error, "code", "command.execute error")?;
            let message = required_string(&error, "message", "command.execute error")?;
            return Err(
                js_sys::Error::new(&format!("command.execute failed: {code}: {message}")).into(),
            );
        }
        let value = Reflect::get(&result, &JsValue::from_str("value"))?;
        if value.is_undefined() {
            return object(&[
                ("kind", JsValue::from_str("error")),
                (
                    "text",
                    JsValue::from_str(&format!("unknown or malformed command: {line}")),
                ),
            ])
            .map(Into::into);
        }
        let events = required(&ctx, "events", "Client Context")?;
        let dispatch = required_function(&events, "dispatch", "events")?;
        let arguments = Array::new();
        arguments.push(&JsValue::from_str("command/executed"));
        arguments.push(&JsValue::from_str(&id));
        arguments.push(&JsValue::from_str(&submitted_command_name(&line)));
        arguments.push(&required(&value, "result", "command execution")?);
        let listeners = dispatch.call2(&events, &JsValue::from_str("emit"), &arguments)?;
        let name = submitted_command_name(&line);
        let command_result = required(&value, "result", "command execution")?;
        for listener in Array::from(&listeners).iter() {
            if let Ok(listener) = listener.dyn_into::<Function>() {
                match listener.call3(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(&id),
                    &JsValue::from_str(&name),
                    &command_result,
                ) {
                    Ok(returned) if !returned.is_null() && !returned.is_undefined() => {
                        match Reflect::get(&returned, &JsValue::from_str("then")) {
                            Ok(then) if then.is_function() => {
                                let promise = Promise::resolve(&returned);
                                let warn_ctx = ctx.clone();
                                let warn_name = name.clone();
                                let warn = Closure::wrap(Box::new(move |error: JsValue| {
                                    warn_executed_listener_failure(&warn_ctx, &warn_name, &error);
                                })
                                    as Box<dyn FnMut(JsValue)>);
                                let _ = promise.catch(&warn);
                                drop(warn.into_js_value());
                            }
                            Ok(_) => {}
                            Err(error) => {
                                warn_executed_listener_failure(&ctx, &name, &error);
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(error) => warn_executed_listener_failure(&ctx, &name, &error),
                }
            }
        }
        object(&[("kind", JsValue::from_str("success"))]).map(Into::into)
    })
}

fn warn_executed_listener_failure(ctx: &JsValue, name: &str, error: &JsValue) {
    let Ok(logger) = required(ctx, "logger", "Client Context") else {
        return;
    };
    let _ = call_method(
        &logger,
        "warn",
        &[
            JsValue::from_str("client command: a command/executed listener for \"%s\" failed"),
            JsValue::from_str(name),
        ],
    );
    let _ = call_method(&logger, "warn", std::slice::from_ref(error));
}
