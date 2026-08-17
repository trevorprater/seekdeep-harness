//! Dispatch-time reconstructability checks for loop-built LLM requests.

use std::sync::Arc;

use futures::stream;
use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_core::{request_header::EpochHeader, session::SessionId, session_store::SessionStore};
use seekdeep_llm::{
    GenerateOptions, LlmCallConfig, LlmRuntime, LlmStreamMiddleware, call_config_equals,
    is_agent_loop_request,
};

/// Validates that a marked request is exactly reconstructable from its live
/// session at the dispatch boundary. Hand-built requests are ignored.
///
/// # Errors
///
/// Returns a precise invariant failure for a missing identity, missing durable
/// boundary, message divergence, or header divergence.
pub fn validate_agent_loop_request(
    options: &GenerateOptions,
    sessions: &SessionStore,
) -> anyhow::Result<()> {
    if !is_agent_loop_request(options) {
        return Ok(());
    }
    let raw_id = options
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("a loop-built request must carry a session id"))?;
    let id = SessionId::new(raw_id);
    let session = sessions.get(&id).ok_or_else(|| {
        anyhow::anyhow!("a loop-built request must carry a live session id, got \"{raw_id}\"")
    })?;
    let events = session.events();
    anyhow::ensure!(
        events.iter().any(|event| event.event_type == "step/start"),
        "a loop-built request with no step/start in its session log"
    );
    let header = session.request_header().ok_or_else(|| {
        anyhow::anyhow!("a loop-built request with no request/header event in its session log")
    })?;
    anyhow::ensure!(
        options.messages == session.derive_messages(),
        "llm request for session \"{raw_id}\" diverges from the dispatch-time durable derivation (log-reconstruction desync)"
    );
    anyhow::ensure!(
        request_matches_header(options, &header),
        "llm request for session \"{raw_id}\" diverges from the folded request header"
    );
    Ok(())
}

fn request_matches_header(options: &GenerateOptions, header: &EpochHeader) -> bool {
    call_config_equals(
        &LlmCallConfig {
            provider: options.provider.clone(),
            model: options.model.clone(),
            reasoning_effort: options.reasoning_effort.clone(),
            temperature: options.temperature,
            max_tokens: options.max_tokens,
            stop: options.stop.clone(),
        },
        &header.config,
    ) && options.system == header.system
        && serde_json::to_value(options.tools.as_deref().unwrap_or_default()).ok()
            == serde_json::to_value(header.tools.as_deref().unwrap_or_default()).ok()
}

/// Prepends the package-owned invariant to the LLM middleware chain so a
/// short-circuiting replay listener cannot silence it.
///
/// # Errors
///
/// Returns if the owner context cannot own the registration.
pub fn install_request_invariant(
    owner: &Context,
    llm: &LlmRuntime,
    sessions: Arc<SessionStore>,
) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
    let middleware: LlmStreamMiddleware = Arc::new(move |options, next| {
        if let Err(error) = validate_agent_loop_request(&options, &sessions) {
            return seekdeep_llm::LlmStream::new(stream::once(async move { Err(error) }));
        }
        next(options)
    });
    llm.register_stream_middleware(owner, middleware, true)
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use seekdeep_core::{
        session::{AppendOptions, SurfaceOp},
        session_store::CreateSessionOptions,
    };
    use seekdeep_llm::{ContentBlock, Message, MessageSource};
    use serde_json::json;

    use super::*;

    fn request(messages: Vec<Message>, id: Option<&str>) -> GenerateOptions {
        let mut request = GenerateOptions::new(
            seekdeep_llm::ProviderId::new("mock"),
            seekdeep_llm::ModelId::new("m"),
            messages,
        );
        request.session_id = id.map(seekdeep_llm::SessionId::new);
        request
    }

    fn setup() -> (
        Context,
        Arc<SessionStore>,
        Arc<seekdeep_core::session::Session>,
    ) {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("req-check")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn");
        let message = Message::user(
            vec![ContentBlock::Text {
                text: "hi".to_owned(),
            }],
            MessageSource::user(),
        );
        session
            .append(
                "user/message",
                serde_json::to_value(message).expect("message"),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..AppendOptions::default()
                },
            )
            .expect("user message");
        (context, sessions, session)
    }

    fn open_request_boundary(session: &seekdeep_core::session::Session) {
        session
            .append(
                "step/start",
                json!({"turn": 1, "step": 1}),
                AppendOptions::default(),
            )
            .expect("step");
        session
            .append(
                "request/header",
                json!({
                    "header": {"config": {"provider": "mock", "model": "m"}},
                    "reason": "initial"
                }),
                AppendOptions::default(),
            )
            .expect("header");
    }

    #[test]
    fn accepts_exact_boundary_and_skips_unmarked_requests() {
        let (_context, sessions, session) = setup();
        open_request_boundary(&session);
        let exact = request(session.derive_messages(), Some(session.id().as_str()));
        validate_agent_loop_request(&exact, &sessions).expect("ordinary request skipped");
        validate_agent_loop_request(&exact.mark_agent_loop_request(), &sessions)
            .expect("exact loop request");
    }

    #[test]
    fn rejects_missing_identity_boundary_header_and_live_session() {
        let (_context, sessions, session) = setup();
        let missing_id = request(Vec::new(), None).mark_agent_loop_request();
        assert!(
            validate_agent_loop_request(&missing_id, &sessions)
                .expect_err("missing id")
                .to_string()
                .contains("carry a session id")
        );
        let ghost = request(Vec::new(), Some("ghost")).mark_agent_loop_request();
        assert!(
            validate_agent_loop_request(&ghost, &sessions)
                .expect_err("ghost")
                .to_string()
                .contains("live session id")
        );
        let bare = request(Vec::new(), Some(session.id().as_str())).mark_agent_loop_request();
        assert!(
            validate_agent_loop_request(&bare, &sessions)
                .expect_err("no step")
                .to_string()
                .contains("no step/start")
        );
        session
            .append(
                "step/start",
                json!({"turn": 1, "step": 1}),
                AppendOptions::default(),
            )
            .expect("step");
        assert!(
            validate_agent_loop_request(&bare, &sessions)
                .expect_err("no header")
                .to_string()
                .contains("no request/header")
        );
    }

    #[test]
    fn rejects_message_and_header_divergence() {
        let (_context, sessions, session) = setup();
        open_request_boundary(&session);
        let mut divergent = session.derive_messages();
        divergent.push(Message::user(
            vec![ContentBlock::Text {
                text: "phantom".to_owned(),
            }],
            MessageSource::user(),
        ));
        let error = validate_agent_loop_request(
            &request(divergent, Some(session.id().as_str())).mark_agent_loop_request(),
            &sessions,
        )
        .expect_err("message divergence");
        assert!(error.to_string().contains("durable derivation"));

        let mut wrong_header = request(session.derive_messages(), Some(session.id().as_str()));
        wrong_header.model = "other".into();
        let error = validate_agent_loop_request(&wrong_header.mark_agent_loop_request(), &sessions)
            .expect_err("header divergence");
        assert!(error.to_string().contains("folded request header"));
    }

    #[tokio::test]
    async fn installed_invariant_precedes_later_short_circuit_middleware() {
        let (context, sessions, session) = setup();
        open_request_boundary(&session);
        let llm = LlmRuntime::install(&context).expect("llm");
        let later: LlmStreamMiddleware =
            Arc::new(|_options, _next| seekdeep_llm::LlmStream::new(stream::empty()));
        llm.register_stream_middleware(&context, later, false)
            .expect("later");
        install_request_invariant(&context, &llm, sessions).expect("invariant");
        let marked = request(Vec::new(), Some(session.id().as_str())).mark_agent_loop_request();
        let error = llm
            .stream(marked)
            .next()
            .await
            .expect("error item")
            .expect_err("invariant must run first");
        assert!(error.to_string().contains("durable derivation"));
    }

    #[test]
    fn process_local_marker_is_not_serialized() {
        let marked = request(Vec::new(), None).mark_agent_loop_request();
        assert!(marked.is_agent_loop_request());
        let wire = serde_json::to_value(&marked).expect("serialize");
        let restored: GenerateOptions = serde_json::from_value(wire).expect("deserialize");
        assert!(!restored.is_agent_loop_request());
    }
}
