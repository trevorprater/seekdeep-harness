//! UI-backed user-question capability seam with one active provider.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, Agent};
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::{AbortSignal, HarnessError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

/// Typed Cordis slot corresponding to `ctx.userQuestions`.
pub const USER_QUESTIONS: ServiceKey<UserQuestionService> = ServiceKey::new("userQuestions");

/// One selectable answer offered to the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskUserQuestionOption {
    /// User-facing label.
    pub label: String,
    /// Optional extra context rendered by capable UIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Extensible presentation intent; unknown tags and fields remain intact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskUserQuestionIntent {
    /// Presentation tag, currently `plan-review`.
    pub kind: String,
    /// Label whose selection means approval.
    pub approve: String,
    /// Future intent fields preserved losslessly.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One question in a user-questions request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskUserQuestionItem {
    /// Stable caller-provided identity echoed in the answer.
    pub id: String,
    /// Specific question displayed to the user.
    pub question: String,
    /// Optional supporting detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Optional short heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Optional choices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<AskUserQuestionOption>>,
    /// Whether more than one option may be selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    /// Optional presentation-only intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<AskUserQuestionIntent>,
}

/// Answer to one question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskUserQuestionAnswerItem {
    /// Echoed question identity.
    pub id: String,
    /// Selected option labels.
    pub selected: Vec<String>,
    /// Optional free-text answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

/// Structured human answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskUserQuestionAnswer {
    /// Answers keyed by their echoed IDs.
    pub answers: Vec<AskUserQuestionAnswerItem>,
}

/// Request delivered unchanged to the active UI provider.
#[derive(Clone, Debug)]
pub struct AskUserQuestionRequest {
    /// Questions to display.
    pub questions: Vec<AskUserQuestionItem>,
    /// Exact live calling agent, when supplied by an agent tool call.
    pub agent: Option<Arc<Agent>>,
    /// Owning tool or step cancellation signal.
    pub signal: Option<AbortSignal>,
}

/// UI-side provider for user questions.
#[async_trait]
pub trait UserQuestionProvider: Send + Sync + 'static {
    /// Collects one structured answer.
    async fn ask(&self, request: AskUserQuestionRequest) -> anyhow::Result<AskUserQuestionAnswer>;
}

/// Stable user-question failure with JavaScript-compatible name and code.
#[derive(Debug, Error)]
#[error("{inner}")]
pub struct UserQuestionError {
    #[source]
    inner: HarnessError,
}

impl UserQuestionError {
    /// Creates one provider- or service-owned stable failure.
    #[must_use]
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            inner: HarnessError::named("UserQuestionError", message, code),
        }
    }

    /// Stable machine-routable code.
    #[must_use]
    pub fn code(&self) -> &str {
        self.inner.code()
    }

    /// Human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        self.inner.message()
    }
}

struct ProviderEntry {
    id: Uuid,
    provider: Arc<dyn UserQuestionProvider>,
}

/// `ctx.userQuestions`: one active UI provider plus an `ask()` API.
pub struct UserQuestionService {
    context: Context,
    provider: Mutex<Option<ProviderEntry>>,
}

impl std::fmt::Debug for UserQuestionService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserQuestionService")
            .field("has_provider", &self.provider.lock().is_some())
            .finish_non_exhaustive()
    }
}

impl UserQuestionService {
    /// Constructs an unprovided service.
    #[must_use]
    pub fn new(context: Context) -> Arc<Self> {
        Arc::new(Self {
            context,
            provider: Mutex::new(None),
        })
    }

    /// Publishes this exact service on `ctx.userQuestions`.
    ///
    /// # Errors
    ///
    /// Returns ordinary duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(USER_QUESTIONS, self.clone())
    }

    /// Registers the single active provider with exact-generation disposal.
    ///
    /// # Errors
    ///
    /// Returns `DUPLICATE_PROVIDER` without replacing an active provider, or
    /// an ownership failure when the mounting context is inactive.
    pub fn register_provider(
        self: &Arc<Self>,
        context: &Context,
        provider: Arc<dyn UserQuestionProvider>,
    ) -> anyhow::Result<EffectHandle> {
        let id = Uuid::new_v4();
        {
            let mut slot = self.provider.lock();
            if slot.is_some() {
                return Err(UserQuestionError::new(
                    "a user-questions provider is already registered",
                    "DUPLICATE_PROVIDER",
                )
                .into());
            }
            *slot = Some(ProviderEntry { id, provider });
        }
        let weak: Weak<Self> = Arc::downgrade(self);
        let effect = EffectHandle::synchronous("userInteraction.registerProvider()", move || {
            if let Some(service) = weak.upgrade() {
                let mut slot = service.provider.lock();
                if slot.as_ref().is_some_and(|entry| entry.id == id) {
                    *slot = None;
                }
            }
            Ok(())
        });
        if let Err(error) = context.own(effect.clone()) {
            let mut slot = self.provider.lock();
            if slot.as_ref().is_some_and(|entry| entry.id == id) {
                *slot = None;
            }
            return Err(error.into());
        }
        Ok(effect)
    }

    /// Validates and asks the active provider.
    ///
    /// # Errors
    ///
    /// Returns the source-compatible closed error taxonomy or the provider's
    /// own failure unchanged.
    pub async fn ask(
        &self,
        request: AskUserQuestionRequest,
    ) -> anyhow::Result<AskUserQuestionAnswer> {
        self.validate_request(&request)?;
        let provider = self
            .provider
            .lock()
            .as_ref()
            .map(|entry| entry.provider.clone())
            .ok_or_else(|| {
                UserQuestionError::new("no user-questions provider is registered", "NO_PROVIDER")
            })?;
        provider.ask(request).await
    }

    fn validate_request(&self, request: &AskUserQuestionRequest) -> anyhow::Result<()> {
        if request.signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            return Err(UserQuestionError::new(
                "ask_user_question was aborted before the user answered",
                "ASK_ABORTED",
            )
            .into());
        }
        if request.questions.is_empty() {
            return Err(UserQuestionError::new(
                "ask_user_question requires at least one question",
                "EMPTY_QUESTIONS",
            )
            .into());
        }
        if let Some(agent) = &request.agent {
            self.validate_agent(agent)?;
        }
        for question in &request.questions {
            validate_intent(question)?;
        }
        Ok(())
    }

    fn validate_agent(&self, agent: &Arc<Agent>) -> anyhow::Result<()> {
        let Some(agents) = self.context.get(AGENTS) else {
            return Err(UserQuestionError::new(
                "human interaction requires the exact live calling agent when an agent is supplied",
                "CALLER_NOT_LIVE",
            )
            .into());
        };
        let exact_live = agents
            .get(agent.id())
            .is_some_and(|live| Arc::ptr_eq(&live, agent));
        if !exact_live {
            return Err(UserQuestionError::new(
                "human interaction requires the exact live calling agent when an agent is supplied",
                "CALLER_NOT_LIVE",
            )
            .into());
        }
        if !agents.roots().iter().any(|root| Arc::ptr_eq(root, agent)) {
            return Err(UserQuestionError::new(
                "human interaction is unavailable while the calling agent is owned by another live agent; include the unresolved question or decision in the child agent's final result",
                "DELEGATED_CALLER",
            )
            .into());
        }
        Ok(())
    }
}

fn validate_intent(question: &AskUserQuestionItem) -> anyhow::Result<()> {
    let Some(intent) = &question.intent else {
        return Ok(());
    };
    let offered = question
        .options
        .as_ref()
        .is_some_and(|options| options.iter().any(|option| option.label == intent.approve));
    if !offered {
        return Err(UserQuestionError::new(
            format!(
                "question {} declares intent {} whose approve label {:?} names none of its options",
                question.id, intent.kind, intent.approve
            ),
            "BAD_INTENT",
        )
        .into());
    }
    if question.detail.is_none() {
        return Err(UserQuestionError::new(
            format!(
                "question {} declares intent {} without the detail it reviews",
                question.id, intent.kind
            ),
            "BAD_INTENT",
        )
        .into());
    }
    Ok(())
}

/// Installs and publishes the user-question service.
///
/// # Errors
///
/// Returns ordinary service registration failures.
pub fn install(context: &Context) -> anyhow::Result<Arc<UserQuestionService>> {
    let service = UserQuestionService::new(context.clone());
    service.provide(context)?;
    Ok(service)
}

/// Registers the package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-user-questions", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let registration = register_invariant(&registry).unwrap();
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.unwrap();
        register_invariant(&registry).unwrap();
    }
}
