//! Settlement of one one-shot subagent run into a background-task outcome.

use std::sync::Arc;

use seekdeep_llm::assistant_text;
use serde::{Deserialize, Serialize};

use crate::types::{SubagentResult, SubagentRun, SubagentStopReason};

/// A background task's terminal outcome.
///
/// Canonical home: the jobs package; mirrored here until that package lands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum JobOutcome {
    /// The task completed with final text.
    Completed {
        /// Flattened final text.
        output: String,
    },
    /// The task was killed.
    Killed,
    /// The task failed.
    Failed {
        /// Failure detail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Flattens final output blocks to the task's final text.
/// Maps a child result to the task outcome.
#[must_use]
pub fn run_outcome(result: &SubagentResult) -> JobOutcome {
    match result.stop_reason {
        SubagentStopReason::Completed => JobOutcome::Completed {
            output: assistant_text(&result.output),
        },
        SubagentStopReason::Aborted => JobOutcome::Killed,
        SubagentStopReason::Error | SubagentStopReason::MaxTokens | SubagentStopReason::Refusal => {
            JobOutcome::Failed {
                detail: Some(result.stop_reason.as_str().to_owned()),
            }
        }
    }
}

/// Awaits the child result, disposes the run, then returns its task outcome.
pub async fn settle_run(run: &Arc<dyn SubagentRun>) -> JobOutcome {
    let outcome = run_outcome(&run.result().await);
    match run.dispose().await {
        Ok(()) => outcome,
        Err(error) => JobOutcome::Failed {
            detail: Some(match &outcome {
                JobOutcome::Failed {
                    detail: Some(detail),
                } => format!("{detail}; dispose failed: {error}"),
                _ => format!("dispose failed: {error}"),
            }),
        },
    }
}
