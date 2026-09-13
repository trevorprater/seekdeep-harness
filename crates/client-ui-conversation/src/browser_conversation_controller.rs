//! Compiled scope-addressed conversation and browser attachment controller.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use seekdeep_identity::SessionId;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::DraftAttachmentId;

thread_local! {
    static UUID_FACTORY: RefCell<Option<Function>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct ImageUrlEntry {
    session_id: SessionId,
    generation: u64,
    pending: Promise,
}

struct ConversationState {
    input: JsValue,
    blocks: JsValue,
    uuid: Function,
    draft_attachments: RefCell<BTreeMap<DraftAttachmentId, JsValue>>,
    image_urls: RefCell<BTreeMap<String, ImageUrlEntry>>,
    image_generations: RefCell<BTreeMap<SessionId, u64>>,
    created_urls: RefCell<BTreeSet<String>>,
    disposed: Cell<bool>,
}

/// Installs the injected browser draft-id source used by new controllers.
#[wasm_bindgen(js_name = configureClientUiConversationController)]
#[allow(clippy::needless_pass_by_value)]
pub fn configure_client_ui_conversation_controller(uuid_factory: Function) {
    UUID_FACTORY.with(|configured| *configured.borrow_mut() = Some(uuid_factory));
}

/// Unsupported browser-declared image type.
#[wasm_bindgen(js_name = UnsupportedImageMediaTypeError)]
pub struct BrowserUnsupportedImageMediaTypeError {
    media_type: String,
}

#[wasm_bindgen(js_class = UnsupportedImageMediaTypeError)]
impl BrowserUnsupportedImageMediaTypeError {
    /// Creates one typed unsupported-media diagnostic.
    #[wasm_bindgen(constructor)]
    pub fn new(media_type: String) -> Self {
        Self { media_type }
    }

    /// Error class name.
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> String {
        "UnsupportedImageMediaTypeError".to_owned()
    }

    /// Source-compatible diagnostic text.
    #[wasm_bindgen(getter)]
    pub fn message(&self) -> String {
        format!(
            "unsupported image media type: {}",
            if self.media_type.is_empty() {
                "(empty)"
            } else {
                &self.media_type
            }
        )
    }

    /// Browser-declared MIME value.
    #[wasm_bindgen(getter, js_name = mediaType)]
    pub fn media_type(&self) -> String {
        self.media_type.clone()
    }
}

/// Scope-bound conversation service wrapper over one shared controller state.
#[wasm_bindgen(js_name = ConversationController)]
pub struct BrowserConversationController {
    inner: Rc<ConversationState>,
    ctx: JsValue,
}

impl BrowserConversationController {
    pub(crate) fn into_service_face(self) -> Result<JsValue, JsValue> {
        let ctx = self.ctx.clone();
        let controller: JsValue = self.into();
        Function::new_with_args(
            "controller, ctx",
            r"return new Proxy(controller, {
              get(target, key) {
                if (key === Symbol.for('cordis.service.tracker')) return true;
                if (key === 'ctx') return ctx;
                const value = Reflect.get(target, key, target);
                if (typeof value !== 'function' || key === 'constructor') return value;
                return function (...args) {
                  const bound = target.forContext(this?.ctx ?? ctx);
                  return Reflect.apply(Reflect.get(bound, key, bound), bound, args);
                };
              },
            });",
        )
        .call2(&JsValue::UNDEFINED, &controller, &ctx)
    }
}

#[wasm_bindgen(js_class = ConversationController)]
#[allow(clippy::needless_pass_by_value)] // JavaScript methods own their ABI arguments.
impl BrowserConversationController {
    /// Creates one root-owned controller.
    ///
    /// # Errors
    ///
    /// Returns before UUID configuration, for malformed config, or lifecycle wiring failure.
    #[wasm_bindgen(constructor)]
    pub fn new(ctx: JsValue, config: JsValue) -> Result<Self, JsValue> {
        let uuid = UUID_FACTORY
            .with(|configured| configured.borrow().clone())
            .ok_or_else(|| {
                js_sys::Error::new(
                    "conversation controller requires configureClientUiConversationController(uuid)",
                )
            })?;
        let inner = Rc::new(ConversationState {
            input: required(&config, "input", "ConversationController config")?,
            blocks: required(&config, "blocks", "ConversationController config")?,
            uuid,
            draft_attachments: RefCell::new(BTreeMap::new()),
            image_urls: RefCell::new(BTreeMap::new()),
            image_generations: RefCell::new(BTreeMap::new()),
            created_urls: RefCell::new(BTreeSet::new()),
            disposed: Cell::new(false),
        });
        own_lifecycle(&ctx, &inner)?;
        Ok(Self { inner, ctx })
    }

    /// Returns a context-bound wrapper sharing all controller state.
    #[must_use]
    #[wasm_bindgen(js_name = forContext)]
    pub fn for_context(&self, ctx: JsValue) -> Self {
        Self {
            inner: self.inner.clone(),
            ctx,
        }
    }

    /// Per-session input resolver.
    #[wasm_bindgen(getter)]
    pub fn input(&self) -> JsValue {
        self.inner.input.clone()
    }

    /// Per-session composer block registry.
    #[wasm_bindgen(getter)]
    pub fn blocks(&self) -> JsValue {
        self.inner.blocks.clone()
    }

    /// Sends one text-only queue prompt through the bound session scope.
    pub fn send(&self, text: String) -> Promise {
        let inner = self.inner.clone();
        let ctx = self.ctx.clone();
        future_to_promise(async move {
            let session = scoped_session(&ctx, "send")?;
            let content = Array::of1(
                &object(&[
                    ("type", JsValue::from_str("text")),
                    ("text", JsValue::from_str(&text)),
                ])?
                .into(),
            );
            let result = await_method(
                &session,
                "prompt",
                &[content.into(), JsValue::from_str("queue")],
            )
            .await?;
            require_ok(&result, "conversation.send")?;
            drop(inner);
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Sends ordered draft images and serialized text through one admission.
    #[wasm_bindgen(js_name = sendSession)]
    pub fn send_session(
        &self,
        session: JsValue,
        text: String,
        image_ids: JsValue,
        mode: String,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let ids = parse_ids(&image_ids)?;
            let attachments = draft_images(&inner, &ids);
            if attachments.len() != ids.len() {
                return Err(js_sys::Error::new(
                    "conversation.sendSession: one or more draft images are no longer available",
                )
                .into());
            }
            let files = attachments
                .iter()
                .map(|attachment| required(attachment, "file", "ComposerAttachment"))
                .collect::<Result<Vec<_>, _>>()?;
            let content = serialize_images(&files).await?;
            if !text.is_empty() {
                content.push(
                    object(&[
                        ("type", JsValue::from_str("text")),
                        ("text", JsValue::from_str(&text)),
                    ])?
                    .as_ref(),
                );
            }
            let result = await_method(
                &session,
                "prompt",
                &[content.into(), JsValue::from_str(&mode)],
            )
            .await?;
            require_ok(&result, "conversation.send")?;
            release_draft_attachments(&inner, &attachments)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Creates browser-only draft image descriptors after validating the whole batch.
    ///
    /// # Errors
    ///
    /// Returns for non-array files, unsupported media types, UUID, or object-URL failures.
    #[wasm_bindgen(js_name = createDraftImages)]
    pub fn create_draft_images(&self, files: JsValue) -> Result<Array, JsValue> {
        let files = files.dyn_into::<Array>()?;
        for file in files.iter() {
            image_media_type(&required_string(&file, "type", "image file")?)?;
        }
        let output = Array::new();
        for file in files.iter() {
            let id = self
                .inner
                .uuid
                .call0(&JsValue::UNDEFINED)?
                .as_string()
                .ok_or_else(|| js_sys::TypeError::new("draft UUID factory must return a string"))?;
            let preview = create_object_url(&file)?;
            let attachment: JsValue = object(&[
                ("kind", JsValue::from_str("image")),
                ("id", JsValue::from_str(&id)),
                ("previewUrl", JsValue::from_str(&preview)),
                ("file", file),
            ])?
            .into();
            self.inner
                .draft_attachments
                .borrow_mut()
                .insert(DraftAttachmentId::new(id), attachment.clone());
            self.inner.created_urls.borrow_mut().insert(preview);
            output.push(&attachment);
        }
        Ok(output)
    }

    /// Resolves live draft descriptors in requested order.
    ///
    /// # Errors
    ///
    /// Returns when ids is not an array of strings.
    #[wasm_bindgen(js_name = draftImages)]
    pub fn draft_images(&self, ids: JsValue) -> Result<Array, JsValue> {
        let ids = parse_ids(&ids)?;
        Ok(draft_images(&self.inner, &ids).into_iter().collect())
    }

    /// Releases one browser-owned draft image.
    ///
    /// # Errors
    ///
    /// Returns for non-string ids or URL revocation failures.
    #[wasm_bindgen(js_name = releaseDraftImage)]
    pub fn release_draft_image(&self, id: String) -> Result<(), JsValue> {
        release_draft_id(&self.inner, &DraftAttachmentId::new(id))
    }

    /// Resolves and caches one historical image URL within its rendered session generation.
    ///
    /// # Errors
    ///
    /// Returns for malformed attachment/session faces or synchronous read failures.
    #[wasm_bindgen(js_name = resolveImage)]
    #[allow(clippy::too_many_lines)] // Cache admission, generation guards, URL creation, and rollback are atomic.
    pub fn resolve_image(
        &self,
        session_id: String,
        attachment: JsValue,
    ) -> Result<Promise, JsValue> {
        if self.inner.disposed.get() {
            return Ok(Promise::reject(
                &js_sys::Error::new("conversation.resolveImage: service is disposed").into(),
            ));
        }
        let session_id = SessionId::new(session_id);
        let attachment_id = required_string(&attachment, "attachmentId", "image attachment")?;
        let key = format!("{}:{attachment_id}", session_id.as_str());
        if let Some(entry) = self.inner.image_urls.borrow().get(&key) {
            return Ok(entry.pending.clone());
        }
        let generation = *self
            .inner
            .image_generations
            .borrow()
            .get(&session_id)
            .unwrap_or(&0);
        let sessions = require_sessions(&self.ctx)?;
        let binding = call_method(
            &sessions,
            "binding",
            &[JsValue::from_str(session_id.as_str())],
        )?;
        if binding.is_null() || binding.is_undefined() {
            return Ok(Promise::reject(
                &js_sys::Error::new(&format!(
                    "conversation.resolveImage: unknown session \"{}\"",
                    session_id.as_str()
                ))
                .into(),
            ));
        }
        let session = required(&binding, "session", "Session binding")?;
        let returned = call_method(
            &session,
            "readAttachment",
            &[JsValue::from_str(&attachment_id)],
        )?;
        let read = Promise::resolve(&returned);
        let inner = self.inner.clone();
        let task_key = key.clone();
        let task_session = session_id.clone();
        let pending = future_to_promise(async move {
            let outcome = async {
                let result = JsFuture::from(read).await?;
                if Reflect::get(&result, &JsValue::from_str("ok"))?.as_bool() != Some(true) {
                    let error = required(&result, "error", "attachment read result")?;
                    return Err(js_sys::Error::new(&format!(
                        "{}: {}",
                        required_string(&error, "code", "attachment read error")?,
                        required_string(&error, "message", "attachment read error")?
                    ))
                    .into());
                }
                if inner.disposed.get() {
                    return Err(js_sys::Error::new(
                        "conversation.resolveImage: service was disposed before loading completed",
                    )
                    .into());
                }
                let current = *inner
                    .image_generations
                    .borrow()
                    .get(&task_session)
                    .unwrap_or(&0);
                if current != generation {
                    return Err(js_sys::Error::new(
                        "historical image scope was released before loading completed",
                    )
                    .into());
                }
                let value = required(&result, "value", "attachment read result")?;
                let loaded_attachment = required(&value, "attachment", "attachment read value")?;
                let media_type =
                    required_string(&loaded_attachment, "mediaType", "image attachment")?;
                let data = Uint8Array::from(required(&value, "data", "attachment read value")?);
                let url = historical_url(&data, &media_type)?;
                if url.starts_with("blob:") {
                    inner.created_urls.borrow_mut().insert(url.clone());
                }
                Ok(JsValue::from_str(&url))
            }
            .await;
            if outcome.is_err()
                && inner
                    .image_urls
                    .borrow()
                    .get(&task_key)
                    .is_some_and(|entry| entry.generation == generation)
            {
                inner.image_urls.borrow_mut().remove(&task_key);
            }
            outcome
        });
        self.inner.image_urls.borrow_mut().insert(
            key,
            ImageUrlEntry {
                session_id,
                generation,
                pending: pending.clone(),
            },
        );
        Ok(pending)
    }

    /// Invalidates and releases every historical image URL for one rendered session.
    #[wasm_bindgen(js_name = releaseSessionImages)]
    pub fn release_session_images(&self, session_id: String) {
        release_session_images(&self.inner, &SessionId::new(session_id));
    }

    /// Applies one queue edit/remove/strict-steer operation through the bound session.
    #[wasm_bindgen(js_name = updateQueue)]
    pub fn update_queue(&self, item_id: JsValue, action: JsValue) -> Promise {
        let ctx = self.ctx.clone();
        future_to_promise(async move {
            let session = scoped_session(&ctx, "updateQueue")?;
            let result = await_method(&session, "updateQueue", &[item_id, action.clone()]).await?;
            if Reflect::get(&result, &JsValue::from_str("ok"))?.as_bool() == Some(true) {
                return Ok(JsValue::UNDEFINED);
            }
            let error = required(&result, "error", "queue update result")?;
            let code = required_string(&error, "code", "queue update error")?;
            let kind = required_string(&action, "kind", "queue action")?;
            if kind == "steer"
                && matches!(code.as_str(), "steer-unavailable" | "queue-item-not-found")
            {
                return Ok(JsValue::UNDEFINED);
            }
            Err(js_sys::Error::new(&format!(
                "conversation.updateQueue failed: {code}: {}",
                required_string(&error, "message", "queue update error")?
            ))
            .into())
        })
    }

    /// Cancels the bound session turn without dropping its queue.
    pub fn cancel(&self) -> Promise {
        let ctx = self.ctx.clone();
        future_to_promise(async move {
            let session = scoped_session(&ctx, "cancel")?;
            let result = await_method(&session, "cancel", &[]).await?;
            require_ok(&result, "conversation.cancel")?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Loads one older history page for the bound session.
    #[wasm_bindgen(js_name = loadOlder)]
    pub fn load_older(&self) -> Promise {
        let ctx = self.ctx.clone();
        future_to_promise(async move {
            let session = scoped_session(&ctx, "loadOlder")?;
            await_method(&session, "loadOlder", &[]).await?;
            Ok(JsValue::UNDEFINED)
        })
    }
}

fn own_lifecycle(ctx: &JsValue, inner: &Rc<ConversationState>) -> Result<(), JsValue> {
    let weak = Rc::downgrade(inner);
    let setup = Closure::wrap(Box::new(move || -> JsValue {
        let weak = weak.clone();
        Closure::wrap(Box::new(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.disposed.set(true);
            let urls = std::mem::take(&mut *inner.created_urls.borrow_mut());
            for url in urls {
                let _ = revoke_preview(&url);
            }
            inner.draft_attachments.borrow_mut().clear();
            inner.image_urls.borrow_mut().clear();
            inner.image_generations.borrow_mut().clear();
        }) as Box<dyn FnMut()>)
        .into_js_value()
    }) as Box<dyn FnMut() -> JsValue>);
    call_method(
        ctx,
        "effect",
        &[
            setup.into_js_value(),
            JsValue::from_str("conversation attachment URL cache"),
        ],
    )?;
    Ok(())
}

fn scoped_session(ctx: &JsValue, operation: &str) -> Result<JsValue, JsValue> {
    let sessions = require_sessions(ctx)?;
    let id = call_method(&sessions, "scopeOf", std::slice::from_ref(ctx))?;
    let Some(id) = id.as_string() else {
        return Err(js_sys::Error::new(&format!(
            "conversation.{operation} requires a session scope — address one via ctx.sessions.scope(id).conversation"
        ))
        .into());
    };
    let binding = call_method(&sessions, "binding", &[JsValue::from_str(&id)])?;
    if binding.is_null() || binding.is_undefined() {
        return Err(js_sys::Error::new(&format!(
            "conversation.{operation}: session \"{id}\" resolved no binding"
        ))
        .into());
    }
    required(&binding, "session", "Session binding")
}

fn require_sessions(ctx: &JsValue) -> Result<JsValue, JsValue> {
    let sessions = call_method(ctx, "get", &[JsValue::from_str("sessions")])?;
    if sessions.is_null() || sessions.is_undefined() {
        Err(js_sys::Error::new("conversation: sessions service unavailable").into())
    } else {
        Ok(sessions)
    }
}

async fn await_method(
    receiver: &JsValue,
    method: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let returned = call_method(receiver, method, arguments)?;
    JsFuture::from(Promise::resolve(&returned)).await
}

fn require_ok(result: &JsValue, operation: &str) -> Result<(), JsValue> {
    if Reflect::get(result, &JsValue::from_str("ok"))?.as_bool() == Some(true) {
        return Ok(());
    }
    let error = required(result, "error", "Session result")?;
    Err(js_sys::Error::new(&format!(
        "{operation} failed: {}: {}",
        required_string(&error, "code", "Session error")?,
        required_string(&error, "message", "Session error")?
    ))
    .into())
}

fn draft_images(inner: &ConversationState, ids: &[DraftAttachmentId]) -> Vec<JsValue> {
    let known = inner.draft_attachments.borrow();
    ids.iter().filter_map(|id| known.get(id).cloned()).collect()
}

fn release_draft_attachments(
    inner: &ConversationState,
    attachments: &[JsValue],
) -> Result<(), JsValue> {
    for attachment in attachments {
        let id = required_string(attachment, "id", "ComposerAttachment")?;
        release_draft_id(inner, &DraftAttachmentId::new(id))?;
    }
    Ok(())
}

fn release_draft_id(inner: &ConversationState, id: &DraftAttachmentId) -> Result<(), JsValue> {
    let Some(attachment) = inner.draft_attachments.borrow_mut().remove(id) else {
        return Ok(());
    };
    let url = required_string(&attachment, "previewUrl", "ComposerAttachment")?;
    inner.created_urls.borrow_mut().remove(&url);
    revoke_preview(&url)
}

fn release_session_images(inner: &Rc<ConversationState>, session_id: &SessionId) {
    let generation = inner
        .image_generations
        .borrow()
        .get(session_id)
        .copied()
        .unwrap_or(0)
        .wrapping_add(1);
    inner
        .image_generations
        .borrow_mut()
        .insert(session_id.clone(), generation);
    let keys = inner
        .image_urls
        .borrow()
        .iter()
        .filter(|(_, entry)| &entry.session_id == session_id)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in keys {
        let Some(entry) = inner.image_urls.borrow_mut().remove(&key) else {
            continue;
        };
        let success_inner = inner.clone();
        let success = Closure::wrap(Box::new(move |value: JsValue| {
            if let Some(url) = value.as_string()
                && success_inner.created_urls.borrow_mut().remove(&url)
            {
                let _ = revoke_preview(&url);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let failure = Closure::wrap(Box::new(move |_error: JsValue| {}) as Box<dyn FnMut(JsValue)>);
        let _ = entry.pending.then2(&success, &failure);
        drop(success.into_js_value());
        drop(failure.into_js_value());
    }
}

async fn serialize_images(images: &[JsValue]) -> Result<Array, JsValue> {
    let reads = Array::new();
    let mut metadata = Vec::new();
    for file in images {
        let media_type = image_media_type(&required_string(file, "type", "image file")?)?;
        let name = Reflect::get(file, &JsValue::from_str("name"))?
            .as_string()
            .unwrap_or_default();
        reads.push(&Promise::resolve(&call_method(file, "arrayBuffer", &[])?));
        metadata.push((media_type, name));
    }
    let values = JsFuture::from(Promise::all(&reads))
        .await?
        .dyn_into::<Array>()?;
    let content = Array::new();
    for (index, (media_type, name)) in metadata.into_iter().enumerate() {
        let bytes = Uint8Array::new(&values.get(u32::try_from(index).unwrap_or_default()));
        let mut fields = vec![
            ("type", JsValue::from_str("image")),
            ("mediaType", JsValue::from_str(&media_type)),
            ("data", JsValue::from_str(&STANDARD.encode(bytes.to_vec()))),
        ];
        if !name.is_empty() {
            fields.push(("name", JsValue::from_str(&name)));
        }
        content.push(object(&fields)?.as_ref());
    }
    Ok(content)
}

fn image_media_type(value: &str) -> Result<String, JsValue> {
    if matches!(
        value,
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    ) {
        return Ok(value.to_owned());
    }
    Err(BrowserUnsupportedImageMediaTypeError::new(value.to_owned()).into())
}

fn historical_url(data: &Uint8Array, media_type: &str) -> Result<String, JsValue> {
    let url = required(&js_sys::global(), "URL", "global")?;
    let create = Reflect::get(&url, &JsValue::from_str("createObjectURL"))?;
    if !create.is_function() {
        return Ok(format!(
            "data:{media_type};base64,{}",
            STANDARD.encode(data.to_vec())
        ));
    }
    let blob_constructor = required(&js_sys::global(), "Blob", "global")?.dyn_into::<Function>()?;
    let parts = Array::of1(&data.buffer());
    let blob = Reflect::construct(
        &blob_constructor,
        &Array::of2(
            parts.as_ref(),
            object(&[("type", JsValue::from_str(media_type))])?.as_ref(),
        ),
    )?;
    create
        .dyn_into::<Function>()?
        .call1(&url, &blob)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("URL.createObjectURL must return a string").into())
}

fn create_object_url(value: &JsValue) -> Result<String, JsValue> {
    let url = required(&js_sys::global(), "URL", "global")?;
    required_function(&url, "createObjectURL", "URL")?
        .call1(&url, value)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new("URL.createObjectURL must return a string").into())
}

fn revoke_preview(value: &str) -> Result<(), JsValue> {
    if !value.starts_with("blob:") {
        return Ok(());
    }
    let url = required(&js_sys::global(), "URL", "global")?;
    required_function(&url, "revokeObjectURL", "URL")?.call1(&url, &JsValue::from_str(value))?;
    Ok(())
}

fn parse_ids(value: &JsValue) -> Result<Vec<DraftAttachmentId>, JsValue> {
    let values = value.clone().dyn_into::<Array>()?;
    values
        .iter()
        .map(|value| {
            value
                .as_string()
                .map(DraftAttachmentId::new)
                .ok_or_else(|| js_sys::TypeError::new("draft image ids must be strings").into())
        })
        .collect()
}

fn call_method(value: &JsValue, key: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required_function(value, key, "object")?;
    let arguments: Array = arguments.iter().collect();
    function.apply(value, &arguments)
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?.dyn_into()
}

fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted {key}")).into())
    } else {
        Ok(property)
    }
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}
