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
    let outcome = match run.result().await {
        Ok(result) => run_outcome(&result),
        Err(error) => JobOutcome::Failed {
            detail: Some(error.to_string()),
        },
    };
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

#[cfg(test)]
mod tests {
    use futures::future::BoxFuture;
    use seekdeep_core::session::SessionId;
    use seekdeep_llm::ContentBlock;

    use super::*;

    struct Run {
        result: Result<SubagentResult, String>,
        disposal: Result<(), String>,
    }

    impl SubagentRun for Run {
        fn id(&self) -> &SessionId {
            static ID: std::sync::OnceLock<SessionId> = std::sync::OnceLock::new();
            ID.get_or_init(|| SessionId::new("child"))
        }

        fn local_agent(&self) -> Option<&std::sync::Arc<seekdeep_agent::Agent>> {
            None
        }

        fn result(&self) -> BoxFuture<'static, anyhow::Result<SubagentResult>> {
            let result = self.result.clone();
            Box::pin(async move { result.map_err(anyhow::Error::msg) })
        }

        fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
            let disposal = self.disposal.clone();
            Box::pin(async move { disposal.map_err(anyhow::Error::msg) })
        }
    }

    fn run(
        stop_reason: SubagentStopReason,
        disposal: Result<(), String>,
    ) -> std::sync::Arc<dyn SubagentRun> {
        std::sync::Arc::new(Run {
            result: Ok(SubagentResult {
                output: vec![ContentBlock::Text {
                    text: "partial".to_owned(),
                }],
                structured: None,
                stop_reason,
            }),
            disposal,
        })
    }

    #[tokio::test]
    async fn maps_every_closed_stop_reason_and_disposes_before_returning() {
        assert_eq!(
            settle_run(&run(SubagentStopReason::Completed, Ok(()))).await,
            JobOutcome::Completed {
                output: "partial".to_owned()
            }
        );
        assert_eq!(
            settle_run(&run(SubagentStopReason::Aborted, Ok(()))).await,
            JobOutcome::Killed
        );
        for reason in [
            SubagentStopReason::Error,
            SubagentStopReason::MaxTokens,
            SubagentStopReason::Refusal,
        ] {
            assert_eq!(
                settle_run(&run(reason, Ok(()))).await,
                JobOutcome::Failed {
                    detail: Some(reason.as_str().to_owned())
                }
            );
        }
    }

    #[tokio::test]
    async fn preserves_result_and_disposal_failures_independently() {
        let failed: std::sync::Arc<dyn SubagentRun> = std::sync::Arc::new(Run {
            result: Err("transport gone".to_owned()),
            disposal: Ok(()),
        });
        assert_eq!(
            settle_run(&failed).await,
            JobOutcome::Failed {
                detail: Some("transport gone".to_owned())
            }
        );
        assert_eq!(
            settle_run(&run(
                SubagentStopReason::Completed,
                Err("reap failed".to_owned())
            ))
            .await,
            JobOutcome::Failed {
                detail: Some("dispose failed: reap failed".to_owned())
            }
        );
        let both: std::sync::Arc<dyn SubagentRun> = std::sync::Arc::new(Run {
            result: Err("result failed".to_owned()),
            disposal: Err("reap failed".to_owned()),
        });
        assert_eq!(
            settle_run(&both).await,
            JobOutcome::Failed {
                detail: Some("result failed; dispose failed: reap failed".to_owned())
            }
        );
    }
}
