//! Native protocol dispatcher shared by the package adapter.

use crate::{
    adapter::{BoxPiEventStream, PiExecutionRequest, PiProtocolExecutor},
    anthropic_messages::AnthropicMessagesExecutor,
    bedrock::BedrockExecutor,
    google_generative::GoogleGenerativeExecutor,
    openai_completions::OpenAiCompletionsExecutor,
    openai_responses::OpenAiResponsesExecutor,
};

/// Built-in Rust protocol engine collection.
#[derive(Clone, Debug)]
pub struct NativePiExecutor {
    completions: OpenAiCompletionsExecutor,
    mistral: OpenAiCompletionsExecutor,
    responses: OpenAiResponsesExecutor,
    azure: OpenAiResponsesExecutor,
    codex: OpenAiResponsesExecutor,
    anthropic: AnthropicMessagesExecutor,
    google: GoogleGenerativeExecutor,
    vertex: GoogleGenerativeExecutor,
    bedrock: BedrockExecutor,
}

impl NativePiExecutor {
    /// Creates all native engines over one reusable HTTP client.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            completions: OpenAiCompletionsExecutor::new(http.clone()),
            mistral: OpenAiCompletionsExecutor::new_mistral(http.clone()),
            responses: OpenAiResponsesExecutor::new(http.clone()),
            azure: OpenAiResponsesExecutor::new_azure(http.clone()),
            codex: OpenAiResponsesExecutor::new_codex(http.clone()),
            anthropic: AnthropicMessagesExecutor::new(http.clone()),
            google: GoogleGenerativeExecutor::new(http.clone()),
            vertex: GoogleGenerativeExecutor::new_vertex(http),
            bedrock: BedrockExecutor,
        }
    }

    /// Releases provider resources scoped to a disposed Harness session.
    pub(crate) async fn close_session(&self, session: &seekdeep_llm::SessionId) {
        self.codex.close_codex_session(session.as_str()).await;
    }
}

impl PiProtocolExecutor for NativePiExecutor {
    fn stream(&self, request: PiExecutionRequest) -> anyhow::Result<BoxPiEventStream> {
        match request.model.api.as_str() {
            "openai-completions" => self.completions.stream(request),
            "mistral-conversations" => self.mistral.stream(request),
            "openai-responses" => self.responses.stream(request),
            "azure-openai-responses" => self.azure.stream(request),
            "openai-codex-responses" => self.codex.stream(request),
            "anthropic-messages" => self.anthropic.stream(request),
            "google-generative-ai" => self.google.stream(request),
            "google-vertex" => self.vertex.stream(request),
            "bedrock-converse-stream" => self.bedrock.stream(request),
            api => anyhow::bail!("native pi-ai protocol \"{api}\" is not ported yet"),
        }
    }
}
