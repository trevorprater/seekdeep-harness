//! Browser `PendingWait` compatibility class over the Rust response carrier.

use std::{cell::Cell, rc::Rc};

use js_sys::{Function, Object, Promise, Reflect};
use seekdeep_identity::{RpcId, SessionId};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::future_to_promise;

use crate::{
    PendingKind, PendingWait,
    wasm_session::{js_to_json, json_to_js},
};

enum PendingBacking {
    Native(Rc<PendingWait>),
    Fixture {
        kind: PendingKind,
        key: String,
        rpc_id: RpcId,
        session_id: SessionId,
        payload: JsValue,
        respond: Function,
        settled: Cell<bool>,
    },
}

/// One pending Host interaction render face plus private response carrier.
#[wasm_bindgen(js_name = PendingWait)]
pub struct WasmPendingWait {
    backing: PendingBacking,
}

impl WasmPendingWait {
    pub(crate) fn from_native(wait: Rc<PendingWait>) -> Self {
        Self {
            backing: PendingBacking::Native(wait),
        }
    }
}

#[wasm_bindgen(js_class = PendingWait)]
impl WasmPendingWait {
    /// Creates the public test-fixture form.
    ///
    /// # Errors
    ///
    /// Returns an unknown interaction kind.
    #[wasm_bindgen(constructor)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        kind: String,
        rpc_id: String,
        session_id: String,
        payload: JsValue,
        respond: Function,
    ) -> Result<Self, JsValue> {
        let kind = match kind.as_str() {
            "approval" => PendingKind::Approval,
            "question" => PendingKind::Question,
            _ => {
                return Err(
                    js_sys::Error::new("pending wait kind must be approval or question").into(),
                );
            }
        };
        let rpc_id = RpcId::new(rpc_id);
        let prefix = match kind {
            PendingKind::Approval => 'a',
            PendingKind::Question => 'q',
        };
        Ok(Self {
            backing: PendingBacking::Fixture {
                kind,
                key: format!("{prefix}:{rpc_id}"),
                rpc_id,
                session_id: SessionId::new(session_id),
                payload,
                respond,
                settled: Cell::new(false),
            },
        })
    }

    /// Interaction kind.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        match self.native_kind() {
            PendingKind::Approval => "approval",
            PendingKind::Question => "question",
        }
        .to_owned()
    }

    /// Stable `<prefix>:<rpcId>` render key.
    #[wasm_bindgen(getter)]
    pub fn key(&self) -> String {
        match &self.backing {
            PendingBacking::Native(wait) => wait.key.clone(),
            PendingBacking::Fixture { key, .. } => key.clone(),
        }
    }

    /// Owning Session identity.
    #[wasm_bindgen(getter, js_name = sessionId)]
    pub fn session_id(&self) -> String {
        match &self.backing {
            PendingBacking::Native(wait) => wait.session_id.as_str(),
            PendingBacking::Fixture { session_id, .. } => session_id.as_str(),
        }
        .to_owned()
    }

    /// Requested frame domain fields.
    ///
    /// # Errors
    ///
    /// Returns JSON-to-JavaScript conversion failures.
    #[wasm_bindgen(getter)]
    pub fn payload(&self) -> Result<JsValue, JsValue> {
        match &self.backing {
            PendingBacking::Native(wait) => json_to_js(&wait.payload),
            PendingBacking::Fixture { payload, .. } => Ok(payload.clone()),
        }
    }

    /// Sends one domain-encoded result with the private `rpcId` restored.
    ///
    /// # Errors
    ///
    /// Returns malformed result conversion or synchronous settled diagnostics.
    #[allow(clippy::needless_pass_by_value)]
    pub fn respond(&self, result: JsValue) -> Result<Promise, JsValue> {
        match &self.backing {
            PendingBacking::Native(wait) => {
                let response = wait
                    .respond(js_to_json(&result)?)
                    .map_err(|error| js_sys::Error::new(&error.to_string()))?;
                Ok(future_to_promise(async move {
                    let receipt = response.await.map_err(|error| js_sys::Error::new(&error))?;
                    json_to_js(&receipt)
                }))
            }
            PendingBacking::Fixture {
                key,
                rpc_id,
                respond,
                settled,
                ..
            } => {
                if settled.get() {
                    return Err(js_sys::Error::new(&format!(
                        "pending wait {key} is already settled"
                    ))
                    .into());
                }
                let message = Object::new();
                set(&message, "type", &JsValue::from_str("client-response"))?;
                set(&message, "rpcId", &JsValue::from_str(rpc_id.as_str()))?;
                set(&message, "result", &result)?;
                Ok(Promise::resolve(
                    &respond.call1(&JsValue::UNDEFINED, &message)?,
                ))
            }
        }
    }

    /// Marks authoritative settlement.
    #[wasm_bindgen(js_name = markSettled)]
    pub fn mark_settled(&self) {
        match &self.backing {
            PendingBacking::Native(wait) => wait.mark_settled(),
            PendingBacking::Fixture { settled, .. } => settled.set(true),
        }
    }
}

impl WasmPendingWait {
    fn native_kind(&self) -> PendingKind {
        match &self.backing {
            PendingBacking::Native(wait) => wait.kind,
            PendingBacking::Fixture { kind, .. } => *kind,
        }
    }
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set PendingWait member {key:?}")).into())
    }
}
