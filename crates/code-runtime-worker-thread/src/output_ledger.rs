//! Combined logs/completion/failure output admission.

use seekdeep_code_runtime::{CodeRunFailure, CodeRunFailureKind, CodeRunResult};
use serde_json::Value;

use crate::output_json::{
    json_string_bytes_up_to, json_value_bytes_up_to, truncate_json_string_bytes,
};

/// Result of offering one worker-side log string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogPush {
    /// Fitting entry or fitting prefix emitted to the host.
    pub emitted: Option<String>,
    /// Whether this push crossed the outer cap for the first time.
    pub limit_reached: bool,
}

/// Worker-side eager ordered log capture under the shared JSON-byte cap.
#[derive(Clone, Debug)]
pub struct LogBuffer {
    max_bytes: usize,
    bytes: usize,
    entries: usize,
    truncated: bool,
}

impl LogBuffer {
    /// Creates a log buffer whose accounting includes the surrounding `[]`.
    #[must_use]
    pub const fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            bytes: 2,
            entries: 0,
            truncated: false,
        }
    }

    /// Offers text, retaining a fitting code-point prefix and reporting the
    /// cap only once.
    pub fn push(&mut self, text: &str) -> LogPush {
        if self.truncated {
            return LogPush {
                emitted: None,
                limit_reached: false,
            };
        }
        let separator = usize::from(self.entries > 0);
        let available = self
            .max_bytes
            .saturating_sub(self.bytes.saturating_add(separator));
        if let Some(string_bytes) = json_string_bytes_up_to(text, available) {
            self.bytes += string_bytes + separator;
            self.entries += 1;
            return LogPush {
                emitted: Some(text.to_owned()),
                limit_reached: false,
            };
        }
        self.truncated = true;
        let prefix = truncate_json_string_bytes(text, available);
        if prefix.is_empty() {
            LogPush {
                emitted: None,
                limit_reached: true,
            }
        } else {
            let Some(prefix_bytes) = json_string_bytes_up_to(&prefix, available) else {
                return LogPush {
                    emitted: None,
                    limit_reached: true,
                };
            };
            self.bytes += prefix_bytes + separator;
            self.entries += 1;
            LogPush {
                emitted: Some(prefix),
                limit_reached: true,
            }
        }
    }

    /// Exact bytes left for the completion or failure message.
    #[must_use]
    pub fn remaining_output_bytes(&self) -> usize {
        self.max_bytes.saturating_sub(self.bytes)
    }
}

/// Host-authoritative combined outer-output ledger.
#[derive(Clone, Debug)]
pub struct OutputLedger {
    max_bytes: usize,
    bytes: usize,
    entries: usize,
}

impl OutputLedger {
    /// Creates a ledger initialized with the empty log array's two bytes.
    #[must_use]
    pub const fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            bytes: 2,
            entries: 0,
        }
    }

    /// Admits one exact log or returns false without mutating the ledger.
    pub fn admit(&mut self, text: &str, sink: &mut Vec<String>) -> bool {
        let separator = usize::from(self.entries > 0);
        let available = self
            .max_bytes
            .saturating_sub(self.bytes.saturating_add(separator));
        let Some(string_bytes) = json_string_bytes_up_to(text, available) else {
            return false;
        };
        self.bytes += string_bytes + separator;
        self.entries += 1;
        sink.push(text.to_owned());
        true
    }

    /// Finalizes a successful run against the remaining combined cap.
    #[must_use]
    pub fn success(&self, logs: Vec<String>, value: Option<Value>) -> CodeRunResult {
        if value.as_ref().is_some_and(|value| {
            json_value_bytes_up_to(value, self.max_bytes.saturating_sub(self.bytes)).is_none()
        }) {
            return self.limit(&logs);
        }
        CodeRunResult {
            value,
            logs,
            error: None,
        }
    }

    /// Finalizes a failure, giving output-limit precedence when its diagnostic
    /// does not fit the remaining combined cap.
    #[must_use]
    pub fn failure(&self, logs: Vec<String>, error: CodeRunFailure) -> CodeRunResult {
        if json_string_bytes_up_to(&error.message, self.max_bytes.saturating_sub(self.bytes))
            .is_none()
        {
            return self.limit(&logs);
        }
        CodeRunResult {
            value: None,
            logs,
            error: Some(error),
        }
    }

    /// Builds the explicit overflow result, retaining the longest ordered log
    /// prefix while reserving space for the fixed diagnostic.
    #[must_use]
    pub fn limit(&self, logs: &[String]) -> CodeRunResult {
        let full_message = format!("outer output exceeded {} bytes", self.max_bytes);
        let message_bytes = full_message.len().saturating_add(2);
        let log_budget = self.max_bytes.saturating_sub(message_bytes);
        let mut retained = Vec::new();
        let mut retained_bytes = 2usize;
        for text in logs {
            let separator = usize::from(!retained.is_empty());
            let available = log_budget.saturating_sub(retained_bytes.saturating_add(separator));
            if let Some(string_bytes) = json_string_bytes_up_to(text, available) {
                retained.push(text.clone());
                retained_bytes += string_bytes + separator;
                continue;
            }
            let prefix = truncate_json_string_bytes(text, available);
            if !prefix.is_empty()
                && let Some(prefix_bytes) = json_string_bytes_up_to(&prefix, available)
            {
                retained.push(prefix);
                retained_bytes += prefix_bytes + separator;
            }
            break;
        }
        let available_message = self.max_bytes.saturating_sub(retained_bytes);
        let message = truncate_json_string_bytes(&full_message, available_message);
        CodeRunResult {
            value: None,
            logs: retained,
            error: Some(CodeRunFailure {
                kind: CodeRunFailureKind::OutputLimit,
                message,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn worker_log_buffer_streams_prefix_and_reports_limit_once() {
        let mut logs = LogBuffer::new(14);
        assert_eq!(
            logs.push("abc"),
            LogPush {
                emitted: Some("abc".to_owned()),
                limit_reached: false
            }
        );
        let crossed = logs.push("😀😀");
        assert!(crossed.limit_reached);
        assert_eq!(crossed.emitted, Some("😀".to_owned()));
        assert_eq!(
            logs.push("ignored"),
            LogPush {
                emitted: None,
                limit_reached: false
            }
        );
        assert_eq!(logs.remaining_output_bytes(), 0);
    }

    #[test]
    fn ledger_combines_logs_value_and_diagnostic_exactly() {
        let mut ledger = OutputLedger::new(20);
        let mut logs = Vec::new();
        assert!(ledger.admit("abc", &mut logs));
        assert_eq!(
            ledger.success(logs.clone(), Some(json!(1))).value,
            Some(json!(1))
        );
        let over = ledger.success(logs.clone(), Some(json!("x".repeat(100))));
        assert_eq!(over.error.unwrap().kind, CodeRunFailureKind::OutputLimit);

        let failure = ledger.failure(
            logs,
            CodeRunFailure {
                kind: CodeRunFailureKind::Exception,
                message: "x".repeat(100),
            },
        );
        assert_eq!(failure.error.unwrap().kind, CodeRunFailureKind::OutputLimit);
    }

    #[test]
    fn limit_retains_only_a_prefix_that_leaves_room_for_message() {
        let ledger = OutputLedger::new(48);
        let result = ledger.limit(&["a".repeat(100)]);
        let error = result.error.unwrap();
        assert_eq!(error.kind, CodeRunFailureKind::OutputLimit);
        assert!(
            json_value_bytes_up_to(
                &json!({ "logs": result.logs, "message": error.message }),
                usize::MAX
            )
            .is_some()
        );
    }
}
