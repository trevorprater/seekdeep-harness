//! Canonical typed tool-definition fixtures for repository tests.

use std::sync::Arc;

use seekdeep_llm::ContentBlock;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{
    DefineToolCallPresenter, DefineToolConcurrencyClassifier, DefineToolExecute, DefineToolOptions,
    DefineToolResultPresenter, ToolContentFinalizer, ToolDefinition, define_tool,
};

/// Options for a fixture whose canonical value is its rendered content array.
pub struct ContentToolFixtureOptions<A> {
    inner: DefineToolOptions<A, Vec<ContentBlock>>,
}

impl<A> ContentToolFixtureOptions<A> {
    /// Builds the fixture's mandatory fields.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        execute: DefineToolExecute<A, Vec<ContentBlock>>,
    ) -> Self {
        Self {
            inner: DefineToolOptions::new(
                name,
                description,
                parameters,
                crate::DefineToolOutput::new(
                    json!({"type": "array", "items": {"type": "json"}}),
                    Arc::new(|_, content: &Vec<ContentBlock>| Ok(content.clone())),
                ),
                execute,
            ),
        }
    }

    /// Adds a final content transform.
    #[must_use]
    pub fn finalize_content(mut self, finalizer: ToolContentFinalizer) -> Self {
        self.inner = self.inner.finalize_content(finalizer);
        self
    }

    /// Declares a cooperative timeout.
    #[must_use]
    pub fn timeout_ms(mut self, timeout_ms: f64) -> Self {
        self.inner = self.inner.timeout_ms(timeout_ms);
        self
    }

    /// Adds a typed fail-closed overlap classifier.
    #[must_use]
    pub fn concurrency_safe(mut self, classifier: DefineToolConcurrencyClassifier<A>) -> Self {
        self.inner = self.inner.concurrency_safe(classifier);
        self
    }

    /// Adds a typed replay-safe pending-call presenter.
    #[must_use]
    pub fn present_call(mut self, presenter: DefineToolCallPresenter<A>) -> Self {
        self.inner = self.inner.present_call(presenter);
        self
    }

    /// Adds a typed replay-safe completed-call presenter.
    #[must_use]
    pub fn present_result(mut self, presenter: DefineToolResultPresenter<A>) -> Self {
        self.inner = self.inner.present_result(presenter);
        self
    }
}

/// Defines a test fixture whose canonical JSON value is its content-block list.
///
/// Product tools should declare domain-owned DTOs instead.
///
/// # Errors
///
/// Returns schema or timeout declaration failures.
pub fn define_content_tool_fixture<A>(
    options: ContentToolFixtureOptions<A>,
) -> anyhow::Result<ToolDefinition>
where
    A: DeserializeOwned + Send + Sync + 'static,
{
    define_tool(options.inner)
}

#[cfg(test)]
mod tests {
    use seekdeep_llm::ContentBlock;
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Args {
        text: String,
    }

    #[test]
    fn fixture_declares_explicit_content_array_contract_and_renderer() {
        let fixture = define_content_tool_fixture(ContentToolFixtureOptions::new(
            "fixture",
            "fixture",
            json!({"text": {"type": "string", "required": true}}),
            Arc::new(|args: Args, _| {
                Box::pin(async move { Ok(vec![ContentBlock::Text { text: args.text }]) })
            }),
        ))
        .expect("fixture");
        assert_eq!(
            fixture.output.schema.as_value(),
            &json!({"type": "array", "items": {}})
        );
        let value = json!([{"type": "text", "text": "ok"}]);
        assert_eq!(
            (fixture.output.render)(&json!({"text": "ok"}), &value).expect("render"),
            [ContentBlock::Text {
                text: "ok".to_owned(),
            }]
        );
    }
}
