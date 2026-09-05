//! Real declarative Loader composition for the first-prompt title provider.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::session::{AppendOptions, SurfaceOp};
use seekdeep_llm::{
    AdapterStream, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmRuntime,
    MessageSource, StreamChunk, UserMessage,
};
use seekdeep_loader::PluginCatalog;
use seekdeep_session_title::{SESSION_TITLE, SessionTitleSource};
use serde_json::json;

#[derive(Debug)]
struct LoaderAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for LoaderAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::TextDelta {
                index: 0,
                text: "Loader composed title".to_owned(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

fn catalog() -> PluginCatalog {
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "seekdeep-llm",
            Plugin::new("llm", std::iter::empty::<&str>(), |context, _| {
                Box::pin(async move {
                    LlmRuntime::install(&context)?;
                    Ok(())
                })
            }),
        )
        .expect("register llm");
    catalog
        .register_named("seekdeep-session", seekdeep_core::session_store::plugin())
        .expect("register sessions");
    catalog
        .register_named("seekdeep-session-title", seekdeep_session_title::plugin())
        .expect("register title service");
    catalog
        .register_named(
            "seekdeep-session-title-first-prompt-llm",
            seekdeep_session_title_first_prompt_llm::plugin(),
        )
        .expect("register title provider");
    catalog
}

#[tokio::test]
async fn yaml_loads_required_policy_and_generates_one_provider_title() -> anyhow::Result<()> {
    let context = Context::new();
    let composition = catalog()
        .load_yaml(
            &context,
            concat!(
                "- id: llm\n",
                "  name: seekdeep-llm\n",
                "- id: sessions\n",
                "  name: seekdeep-session\n",
                "- id: titles\n",
                "  name: seekdeep-session-title\n",
                "  config:\n",
                "    fallbackMaxWords: 5\n",
                "    fallbackMaxBytes: 40\n",
                "    maxTitleBytes: 80\n",
                "- id: provider\n",
                "  name: seekdeep-session-title-first-prompt-llm\n",
                "  config:\n",
                "    targetWords: 5\n",
                "    targetCjkCharacters: 10\n",
                "    maxInputBytes: 1000\n",
                "    maxOutputTokens: 32\n",
                "    timeoutMs: 1000\n",
                "    provider: title-route\n",
                "    model: title-model\n",
            ),
        )
        .await?;
    assert_eq!(composition.fibers().len(), 4);

    let adapter = Arc::new(LoaderAdapter {
        requests: Mutex::new(Vec::new()),
    });
    let llm = context
        .get(seekdeep_llm::LLM)
        .ok_or_else(|| anyhow::anyhow!("llm missing"))?;
    llm.register_adapter(&["title-route".to_owned()], adapter.clone())?;
    let sessions = context
        .get(seekdeep_core::session_store::SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("sessions missing"))?;
    let session = sessions.create(
        &context,
        Some(seekdeep_core::session::SessionId::new("loader-title")),
        seekdeep_core::session_store::CreateSessionOptions::default(),
    )?;
    session.append("turn/start", json!({"turn": 1}), AppendOptions::default())?;
    let message = session.append(
        "user/message",
        serde_json::to_value(UserMessage::new(
            vec![ContentBlock::Text {
                text: "Compose a title through Loader".to_owned(),
            }],
            MessageSource::user(),
        ))?,
        AppendOptions {
            surface_op: Some(SurfaceOp::append()),
            ..AppendOptions::default()
        },
    )?;
    session.append(
        "request/header",
        json!({
            "header": {"config": {"provider": "main-route", "model": "main-model"}},
            "reason": "initial",
        }),
        AppendOptions::default(),
    )?;

    let titles = context
        .get(SESSION_TITLE)
        .ok_or_else(|| anyhow::anyhow!("title service missing"))?;
    let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(snapshot) = titles.get(&session)
                && snapshot.event.title == "Loader composed title"
            {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;

    {
        let requests = adapter.requests.lock();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].provider.as_str(), "title-route");
        assert_eq!(requests[0].model.as_str(), "title-model");
    }
    assert_eq!(snapshot.event.message_seqs, [message.seq]);
    assert_eq!(snapshot.event.title, "Loader composed title");
    assert!(matches!(
        snapshot.event.source,
        SessionTitleSource::Provider {
            ref provider,
            model: Some(ref model),
        } if provider.as_str() == "session-title-first-prompt-llm"
            && model.provider == "title-route"
            && model.model == "title-model"
    ));
    composition.dispose().await?;
    context.fiber().dispose().await
}
