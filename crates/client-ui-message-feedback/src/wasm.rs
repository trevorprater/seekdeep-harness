//! Browser WASM controller, plugin, and controls.

use std::{cell::RefCell, rc::Rc};

use futures::FutureExt as _;
use js_sys::{Array, Function, Map, Object, Promise, Reflect};
use serde::de::DeserializeOwned;
use serde_json::Value;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

use crate::{
    FeedbackCarrierFailure, FeedbackMessageId, FeedbackRemoteResult, FeedbackSessionId,
    FeedbackSubscription, MessageFeedbackActionResult, MessageFeedbackController,
    MessageFeedbackFailure, MessageFeedbackItem, MessageFeedbackRating, MessageFeedbackRemote,
    MessageFeedbackVersion, MessageFeedbackView,
};

thread_local! {
    static STABLE_ACTION_RESULTS: RefCell<Option<(JsValue, JsValue)>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct JsRemote {
    remote: JsValue,
}

impl MessageFeedbackRemote for JsRemote {
    fn list(
        &self,
        session_id: FeedbackSessionId,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<FeedbackRemoteResult<Vec<MessageFeedbackItem>>, String>,
    > {
        let remote = self.remote.clone();
        async move {
            let request = js_object(&[("sessionId", JsValue::from_str(session_id.as_str()))])?;
            let value = await_method(
                &remote,
                "list",
                &[request.into()],
                "message feedback list failed",
            )
            .await?;
            parse_list_envelope(value)
        }
        .boxed_local()
    }

    fn put(
        &self,
        session_id: FeedbackSessionId,
        message_id: FeedbackMessageId,
        rating: MessageFeedbackRating,
        note: Option<String>,
        if_version: Option<MessageFeedbackVersion>,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<FeedbackRemoteResult<MessageFeedbackItem>, String>,
    > {
        let remote = self.remote.clone();
        async move {
            let request = js_object(&[
                ("sessionId", JsValue::from_str(session_id.as_str())),
                ("messageId", JsValue::from_str(message_id.as_str())),
                (
                    "rating",
                    JsValue::from_str(match rating {
                        MessageFeedbackRating::Positive => "positive",
                        MessageFeedbackRating::Negative => "negative",
                    }),
                ),
                (
                    "ifVersion",
                    if_version.map_or(JsValue::NULL, |version| JsValue::from_str(&version.0)),
                ),
            ])?;
            if let Some(note) = note {
                Reflect::set(
                    &request,
                    &JsValue::from_str("note"),
                    &JsValue::from_str(&note),
                )
                .map_err(|error| js_string(&error))?;
            }
            let value = await_method(
                &remote,
                "put",
                &[request.into()],
                "message feedback mutation failed",
            )
            .await?;
            parse_item_envelope(value)
        }
        .boxed_local()
    }

    fn delete(
        &self,
        session_id: FeedbackSessionId,
        message_id: FeedbackMessageId,
        if_version: MessageFeedbackVersion,
    ) -> futures::future::LocalBoxFuture<'static, Result<FeedbackRemoteResult<()>, String>> {
        let remote = self.remote.clone();
        async move {
            let request = js_object(&[
                ("sessionId", JsValue::from_str(session_id.as_str())),
                ("messageId", JsValue::from_str(message_id.as_str())),
                ("ifVersion", JsValue::from_str(&if_version.0)),
            ])?;
            let value = await_method(
                &remote,
                "delete",
                &[request.into()],
                "message feedback mutation failed",
            )
            .await?;
            parse_delete_envelope(value)
        }
        .boxed_local()
    }
}

struct BrowserController {
    controller: MessageFeedbackController,
    snapshot_cache: RefCell<Option<(Rc<MessageFeedbackView>, JsValue)>>,
    subscribers: RefCell<Vec<(Function, FeedbackSubscription)>>,
}

/// JavaScript-facing per-Session feedback controller.
#[wasm_bindgen(js_name = MessageFeedbackController)]
pub struct WasmMessageFeedbackController {
    inner: Rc<BrowserController>,
}

#[wasm_bindgen(js_class = MessageFeedbackController)]
impl WasmMessageFeedbackController {
    /// Creates a cold controller over the generated Remote namespace.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(remote: JsValue, session_id: String) -> Self {
        Self {
            inner: Rc::new(BrowserController {
                controller: MessageFeedbackController::new(
                    Rc::new(JsRemote { remote }),
                    FeedbackSessionId::new(session_id),
                ),
                snapshot_cache: RefCell::new(None),
                subscribers: RefCell::new(Vec::new()),
            }),
        }
    }

    /// Stable immutable view until the next publish.
    ///
    /// # Errors
    ///
    /// Returns JavaScript object-construction failures.
    #[wasm_bindgen(js_name = getSnapshot)]
    pub fn get_snapshot(&self) -> Result<JsValue, JsValue> {
        browser_snapshot(&self.inner)
    }

    /// Subscribes to view replacement.
    #[allow(clippy::needless_pass_by_value)]
    pub fn subscribe(&self, listener: Function) -> Function {
        if !self
            .inner
            .subscribers
            .borrow()
            .iter()
            .any(|(current, _)| Object::is(current.as_ref(), listener.as_ref()))
        {
            let callback = listener.clone();
            let subscription = self.inner.controller.subscribe(Rc::new(move || {
                if let Err(error) = callback.call0(&JsValue::UNDEFINED) {
                    log_error("[ui-message-feedback] subscriber threw:", &error);
                }
            }));
            self.inner
                .subscribers
                .borrow_mut()
                .push((listener.clone(), subscription));
        }
        browser_disposer(self.inner.clone(), listener)
    }

    /// Loads until ready.
    pub fn ensure(&self) -> Promise {
        action_promise(self.inner.controller.clone(), ControllerAction::Ensure)
    }

    /// Forces one retryable list read.
    pub fn refresh(&self) -> Promise {
        action_promise(self.inner.controller.clone(), ControllerAction::Refresh)
    }

    /// Serializes one reconnect read behind mutations.
    pub fn resync(&self) -> Promise {
        action_promise(self.inner.controller.clone(), ControllerAction::Resync)
    }

    /// Creates or replaces one rating.
    #[allow(clippy::needless_pass_by_value)]
    pub fn rate(&self, message_id: String, rating: String, note: JsValue) -> Promise {
        action_promise(
            self.inner.controller.clone(),
            ControllerAction::Rate {
                message_id,
                rating,
                note: note.as_string(),
            },
        )
    }

    /// Toggles one rating.
    pub fn toggle(&self, message_id: String, rating: String) -> Promise {
        action_promise(
            self.inner.controller.clone(),
            ControllerAction::Toggle { message_id, rating },
        )
    }

    /// Removes only the note.
    #[wasm_bindgen(js_name = clearNote)]
    pub fn clear_note(&self, message_id: String) -> Promise {
        action_promise(
            self.inner.controller.clone(),
            ControllerAction::ClearNote { message_id },
        )
    }

    /// Removes the item.
    pub fn clear(&self, message_id: String) -> Promise {
        action_promise(
            self.inner.controller.clone(),
            ControllerAction::Clear { message_id },
        )
    }

    /// Drops subscribers and refuses newly admitted work.
    pub fn dispose(&self) {
        self.inner.controller.dispose();
        self.inner.subscribers.borrow_mut().clear();
    }
}

enum ControllerAction {
    Ensure,
    Refresh,
    Resync,
    Rate {
        message_id: String,
        rating: String,
        note: Option<String>,
    },
    Toggle {
        message_id: String,
        rating: String,
    },
    ClearNote {
        message_id: String,
    },
    Clear {
        message_id: String,
    },
}

fn action_promise(controller: MessageFeedbackController, action: ControllerAction) -> Promise {
    future_to_promise(async move {
        let result = match action {
            ControllerAction::Ensure => controller.ensure().await,
            ControllerAction::Refresh => controller.refresh().await,
            ControllerAction::Resync => controller.resync().await,
            ControllerAction::Rate {
                message_id,
                rating,
                note,
            } => {
                let Some(rating) = parse_rating(&rating) else {
                    return Err(js_sys::TypeError::new(
                        "feedback rating must be positive or negative",
                    )
                    .into());
                };
                controller
                    .rate(FeedbackMessageId::new(message_id), rating, note)
                    .await
            }
            ControllerAction::Toggle { message_id, rating } => {
                let Some(rating) = parse_rating(&rating) else {
                    return Err(js_sys::TypeError::new(
                        "feedback rating must be positive or negative",
                    )
                    .into());
                };
                controller
                    .toggle(FeedbackMessageId::new(message_id), rating)
                    .await
            }
            ControllerAction::ClearNote { message_id } => {
                controller
                    .clear_note(FeedbackMessageId::new(message_id))
                    .await
            }
            ControllerAction::Clear { message_id } => {
                controller.clear(FeedbackMessageId::new(message_id)).await
            }
        };
        action_result_to_js(&result)
    })
}

fn browser_snapshot(inner: &Rc<BrowserController>) -> Result<JsValue, JsValue> {
    let snapshot = inner.controller.snapshot();
    if let Some((cached, value)) = inner.snapshot_cache.borrow().as_ref()
        && Rc::ptr_eq(cached, &snapshot)
    {
        return Ok(value.clone());
    }
    let items = Map::new();
    for (message_id, item) in &snapshot.items {
        items.set(&JsValue::from_str(message_id.as_str()), &item_to_js(item)?);
    }
    let value = browser_object(&[
        (
            "status",
            JsValue::from_str(match snapshot.status {
                crate::MessageFeedbackStatus::Cold => "cold",
                crate::MessageFeedbackStatus::Loading => "loading",
                crate::MessageFeedbackStatus::Ready => "ready",
                crate::MessageFeedbackStatus::Error => "error",
            }),
        ),
        ("items", items.into()),
        (
            "error",
            snapshot
                .error
                .as_deref()
                .map_or(JsValue::NULL, JsValue::from_str),
        ),
    ])?;
    Object::freeze(&value);
    let value: JsValue = value.into();
    *inner.snapshot_cache.borrow_mut() = Some((snapshot, value.clone()));
    Ok(value)
}

fn item_to_js(item: &MessageFeedbackItem) -> Result<JsValue, JsValue> {
    let value = serde_wasm_bindgen::to_value(item)?;
    Object::freeze(&Object::from(value.clone()));
    Ok(value)
}

fn action_result_to_js(result: &MessageFeedbackActionResult) -> Result<JsValue, JsValue> {
    match result {
        MessageFeedbackActionResult::Ok => stable_action_result(false),
        MessageFeedbackActionResult::Error { code, message }
            if code == "disposed" && message == "feedback controller is disposed" =>
        {
            stable_action_result(true)
        }
        MessageFeedbackActionResult::Error { code, message } => browser_object(&[
            ("ok", JsValue::FALSE),
            (
                "error",
                browser_object(&[
                    ("code", JsValue::from_str(code)),
                    ("message", JsValue::from_str(message)),
                ])?
                .into(),
            ),
        ])
        .map(Into::into),
    }
}

fn stable_action_result(disposed: bool) -> Result<JsValue, JsValue> {
    STABLE_ACTION_RESULTS.with(|cache| {
        if cache.borrow().is_none() {
            let ok = browser_object(&[("ok", JsValue::TRUE)])?;
            Object::freeze(&ok);
            let error = browser_object(&[
                ("code", JsValue::from_str("disposed")),
                (
                    "message",
                    JsValue::from_str("feedback controller is disposed"),
                ),
            ])?;
            Object::freeze(&error);
            let disposed = browser_object(&[("ok", JsValue::FALSE), ("error", error.into())])?;
            Object::freeze(&disposed);
            *cache.borrow_mut() = Some((ok.into(), disposed.into()));
        }
        let cache = cache.borrow();
        let (ok, disposed_value) = cache.as_ref().expect("initialized");
        Ok(if disposed {
            disposed_value.clone()
        } else {
            ok.clone()
        })
    })
}

fn browser_disposer(inner: Rc<BrowserController>, listener: Function) -> Function {
    Closure::wrap(Box::new(move || {
        let index = inner
            .subscribers
            .borrow()
            .iter()
            .position(|(current, _)| Object::is(current.as_ref(), listener.as_ref()));
        if let Some(index) = index {
            inner.subscribers.borrow_mut().remove(index);
        }
    }) as Box<dyn FnMut()>)
    .into_js_value()
    .unchecked_into()
}

fn parse_list_envelope(
    value: JsValue,
) -> Result<FeedbackRemoteResult<Vec<MessageFeedbackItem>>, String> {
    let value = js_json(value)?;
    let business = carrier_value(&value)?;
    match business {
        Err(error) => Ok(Err(error)),
        Ok(business) => match business_value(&business)? {
            Err(error) => Ok(Ok(Err(error))),
            Ok(value) => {
                let items = value
                    .get("items")
                    .cloned()
                    .ok_or_else(|| "feedback list omitted items".to_owned())?;
                Ok(Ok(Ok(parse_json(items)?)))
            }
        },
    }
}

fn parse_item_envelope(
    value: JsValue,
) -> Result<FeedbackRemoteResult<MessageFeedbackItem>, String> {
    let value = js_json(value)?;
    let business = carrier_value(&value)?;
    match business {
        Err(error) => Ok(Err(error)),
        Ok(business) => match business_value(&business)? {
            Err(error) => Ok(Ok(Err(error))),
            Ok(value) => Ok(Ok(Ok(parse_json(value)?))),
        },
    }
}

fn parse_delete_envelope(value: JsValue) -> Result<FeedbackRemoteResult<()>, String> {
    let value = js_json(value)?;
    let business = carrier_value(&value)?;
    match business {
        Err(error) => Ok(Err(error)),
        Ok(business) => match business_value(&business)? {
            Err(error) => Ok(Ok(Err(error))),
            Ok(_) => Ok(Ok(Ok(()))),
        },
    }
}

fn carrier_value(value: &Value) -> Result<Result<Value, FeedbackCarrierFailure>, String> {
    let ok = value
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "Remote envelope omitted ok".to_owned())?;
    if ok {
        Ok(Ok(value.get("value").cloned().unwrap_or(Value::Null)))
    } else {
        Ok(Err(parse_json(
            value.get("error").cloned().unwrap_or(Value::Null),
        )?))
    }
}

fn business_value(value: &Value) -> Result<Result<Value, MessageFeedbackFailure>, String> {
    let ok = value
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "feedback business result omitted ok".to_owned())?;
    if ok {
        Ok(Ok(value.get("value").cloned().unwrap_or(Value::Null)))
    } else {
        Ok(Err(parse_json(
            value.get("error").cloned().unwrap_or(Value::Null),
        )?))
    }
}

fn parse_json<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn js_json(value: JsValue) -> Result<Value, String> {
    serde_wasm_bindgen::from_value(value).map_err(|error| error.to_string())
}

async fn await_method(
    value: &JsValue,
    name: &str,
    arguments: &[JsValue],
    non_error_fallback: &str,
) -> Result<JsValue, String> {
    let result = call_method(value, name, arguments)
        .map_err(|error| transport_message(&error, non_error_fallback))?;
    JsFuture::from(Promise::resolve(&result))
        .await
        .map_err(|error| transport_message(&error, non_error_fallback))
}

fn parse_rating(value: &str) -> Option<MessageFeedbackRating> {
    match value {
        "positive" => Some(MessageFeedbackRating::Positive),
        "negative" => Some(MessageFeedbackRating::Negative),
        _ => None,
    }
}

fn js_object(entries: &[(&str, JsValue)]) -> Result<Object, String> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value).map_err(|error| js_string(&error))?;
    }
    Ok(object)
}

fn browser_object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    js_object(entries).map_err(|message| js_sys::Error::new(&message).into())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let callable = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let args: Array = arguments.iter().collect();
    callable.apply(value, &args)
}

fn js_string(value: &JsValue) -> String {
    value
        .dyn_ref::<js_sys::Error>()
        .map(|error| String::from(error.message()))
        .or_else(|| value.as_string())
        .unwrap_or_else(|| "message feedback transport failed".to_owned())
}

fn transport_message(value: &JsValue, non_error_fallback: &str) -> String {
    value.dyn_ref::<js_sys::Error>().map_or_else(
        || non_error_fallback.to_owned(),
        |error| String::from(error.message()),
    )
}

fn log_error(message: &str, error: &JsValue) {
    let global = js_sys::global();
    if let Ok(console) = Reflect::get(&global, &JsValue::from_str("console"))
        && let Ok(log) = Reflect::get(&console, &JsValue::from_str("error"))
            .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
    {
        let _ = log.call2(&console, &JsValue::from_str(message), error);
    }
}
