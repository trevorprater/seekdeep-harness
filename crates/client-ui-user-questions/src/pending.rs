//! Question-domain response encoding over the runtime pending carrier.

use std::rc::Rc;

use futures::future::LocalBoxFuture;
use seekdeep_identity::SessionId;
use seekdeep_user_questions_contract::{AskUserQuestionAnswer, AskUserQuestionItem};
use serde::Deserialize;
use serde_json::{Value, json};

#[cfg(not(target_arch = "wasm32"))]
use seekdeep_client_runtime::PendingWait;

#[derive(Deserialize)]
struct QuestionPayload {
    questions: Vec<AskUserQuestionItem>,
}

/// Target-portable carrier required by the question domain face.
pub trait QuestionCarrier: std::fmt::Debug {
    /// Opaque render identity.
    fn key(&self) -> &str;
    /// Owning Session identity.
    fn session_id(&self) -> &SessionId;
    /// Requested frame domain fields.
    fn payload(&self) -> &Value;
    /// Sends one domain-encoded result.
    ///
    /// # Errors
    ///
    /// Returns synchronous authoritative-settlement failures.
    fn respond(
        &self,
        result: Value,
    ) -> Result<LocalBoxFuture<'static, Result<Value, String>>, String>;
}

#[cfg(not(target_arch = "wasm32"))]
impl QuestionCarrier for PendingWait {
    fn key(&self) -> &str {
        &self.key
    }

    fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    fn payload(&self) -> &Value {
        &self.payload
    }

    fn respond(
        &self,
        result: Value,
    ) -> Result<LocalBoxFuture<'static, Result<Value, String>>, String> {
        PendingWait::respond(self, result).map_err(|error| error.to_string())
    }
}

/// Question domain face over one immutable target-specific carrier.
#[derive(Clone, Debug)]
pub struct PendingQuestion {
    wait: Rc<dyn QuestionCarrier>,
}

impl PendingQuestion {
    /// Mints one domain face for a stable carrier identity.
    #[must_use]
    pub fn new<C: QuestionCarrier + 'static>(wait: Rc<C>) -> Self {
        Self { wait }
    }

    /// Opaque render identity forwarded from the carrier.
    #[must_use]
    pub fn key(&self) -> &str {
        self.wait.key()
    }

    /// Owning Session identity forwarded from the carrier.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        self.wait.session_id()
    }

    /// Parses the carrier's question batch.
    ///
    /// # Errors
    ///
    /// Returns a schema error when an untrusted carrier omitted or malformed the batch.
    pub fn questions(&self) -> Result<Vec<AskUserQuestionItem>, serde_json::Error> {
        serde_json::from_value::<QuestionPayload>(self.wait.payload().clone())
            .map(|payload| payload.questions)
    }

    /// Delivers the complete answer batch and rejects a negative receipt.
    ///
    /// # Errors
    ///
    /// Returns synchronous settlement, transport, or exact receipt-rejection text.
    pub async fn answer(&self, answer: AskUserQuestionAnswer) -> Result<(), String> {
        self.respond(
            json!({
                "ok": true,
                "value": {
                    "sessionId": self.wait.session_id(),
                    "answer": answer,
                },
            }),
            "response",
        )
        .await
    }

    /// Cancels the whole request and rejects a negative receipt.
    ///
    /// # Errors
    ///
    /// Returns synchronous settlement, transport, or exact receipt-rejection text.
    pub async fn cancel(&self) -> Result<(), String> {
        self.respond(
            json!({
                "ok": false,
                "error": {
                    "code": "cancelled",
                    "message": "the user closed this question request",
                    "details": {},
                },
            }),
            "cancellation",
        )
        .await
    }

    async fn respond(&self, result: Value, operation: &str) -> Result<(), String> {
        let receipt = self.wait.respond(result)?.await?;
        if receipt.get("accepted").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let reason = receipt
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("undefined");
        Err(format!("question {operation} rejected: {reason}"))
    }
}
