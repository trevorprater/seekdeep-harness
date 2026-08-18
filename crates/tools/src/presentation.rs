//! Provider-neutral render intents for pending and completed tool calls.

use seekdeep_llm::ContentBlock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Category used by clients to choose a call icon or treatment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallKind {
    /// Reads state without mutation.
    Read,
    /// Edits existing state.
    Edit,
    /// Deletes state.
    Delete,
    /// Moves or renames state.
    Move,
    /// Searches for state.
    Search,
    /// Executes a command or program.
    Execute,
    /// Fetches a remote resource.
    Fetch,
    /// No more specific category applies.
    #[default]
    Other,
}

/// A model-facing file path and optional one-based focus line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileLocation {
    /// Path the tool operated on.
    pub path: String,
    /// Optional one-based line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// One file change for inline-diff presentation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileDiff {
    /// Changed path.
    pub path: String,
    /// Prior content, or null when unavailable/new.
    pub old_text: Option<String>,
    /// Content after the change.
    pub new_text: String,
}

/// Generic pending-call card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenericCallView {
    /// Always-visible call label.
    pub title: String,
    /// Optional category; clients default it to other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolCallKind>,
    /// Salient raw input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
    /// Additional pending-state content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    /// Files a capable client may follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<FileLocation>>,
}

/// Terminal pending-call card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalCallView {
    /// Command shown as the terminal title.
    pub title: String,
    /// One-line summary shown above the terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Absolute or workspace-relative working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Diff pending-call card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiffCallView {
    /// Card header.
    pub title: String,
    /// Changes in file order.
    pub diffs: Vec<FileDiff>,
    /// Files a capable client may follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<FileLocation>>,
}

/// Provider-neutral pending-call render intent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "card", rename_all = "lowercase")]
pub enum ToolCallView {
    /// Generic call card.
    Generic(GenericCallView),
    /// Terminal call card.
    Terminal(TerminalCallView),
    /// Inline-diff call card.
    Diff(DiffCallView),
}

/// One numbered source line returned by a read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadFileLine {
    /// One-based file line number.
    pub number: u64,
    /// Line text without its trailing newline.
    pub text: String,
}

/// Generic completed-call card.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenericResultView {
    /// Replacement title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Reprojected content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
}

/// Terminal completed-call card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalResultView {
    /// Replacement title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Captured output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Process exit code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Terminating signal name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

/// Diff completed-call card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiffResultView {
    /// Replacement title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Applied changes in file order.
    pub diffs: Vec<FileDiff>,
}

/// One matched source line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchLineMatch {
    /// One-based line number.
    pub line_number: u64,
    /// Surfaced line preview.
    pub line: String,
}

/// One file's grouped content matches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchFileMatches {
    /// Display path.
    pub path: String,
    /// Matches in output order.
    pub matches: Vec<SearchLineMatch>,
}

/// Grouped content-search result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchMatchesResultView {
    /// Replacement title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Retained groups in first-seen order.
    pub files: Vec<SearchFileMatches>,
    /// Whether results were capped.
    pub truncated: bool,
    /// Total matches before capping.
    pub total: u64,
}

/// Flat path-search result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchPathsResultView {
    /// Replacement title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Retained paths.
    pub paths: Vec<String>,
    /// Whether results were capped.
    pub truncated: bool,
    /// Total paths before capping.
    pub total: u64,
}

/// Search result shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "lowercase")]
pub enum SearchResultView {
    /// Content matches grouped by file.
    Matches(SearchMatchesResultView),
    /// Flat path list.
    Paths(SearchPathsResultView),
}

/// Completed file-read card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadResultView {
    /// Replacement title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Model-facing path.
    pub path: String,
    /// One-based first requested line.
    pub offset: u64,
    /// Returned line window.
    pub lines: Vec<ReadFileLine>,
    /// Exact file line count.
    pub total_lines: u64,
    /// Syntax-highlighting hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Generic-client fallback content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
}

/// Citeable web-search source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSource {
    /// Source URL.
    pub url: String,
    /// Optional source title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional excerpt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Provider timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

/// Completed web-search card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchResultView {
    /// Replacement title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Faithful structured sources.
    pub sources: Vec<WebSource>,
    /// Optional provider answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Whether sources were capped.
    pub truncated: bool,
}

/// Completed web-fetch card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebFetchResultView {
    /// Replacement title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Final URL after redirects.
    pub url: String,
    /// HTTP status.
    pub status_code: u16,
    /// Effective truncation state.
    pub truncated: bool,
}

/// Web result shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WebResultView {
    /// Web-search result.
    Search(WebSearchResultView),
    /// Web-fetch result.
    Fetch(WebFetchResultView),
}

/// Provider-neutral completed-call render intent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "card", rename_all = "lowercase")]
pub enum ToolResultView {
    /// Generic result card.
    Generic(GenericResultView),
    /// Terminal result card.
    Terminal(TerminalResultView),
    /// Diff result card.
    Diff(DiffResultView),
    /// Search result card.
    Search(SearchResultView),
    /// Read result card.
    Read(ReadResultView),
    /// Web result card.
    Web(WebResultView),
}

/// Durable result projection handed to replay-safe presenters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolResult {
    /// Final model-facing content.
    pub content: Vec<ContentBlock>,
    /// Whether the call failed.
    pub is_error: bool,
    /// Tool-private presentation metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn pending_views_use_the_exact_card_tagged_wire_shapes() {
        let generic = ToolCallView::Generic(GenericCallView {
            title: "Open /a".to_owned(),
            kind: Some(ToolCallKind::Read),
            raw_input: Some(json!("/a")),
            content: None,
            locations: Some(vec![FileLocation {
                path: "/a".to_owned(),
                line: Some(3),
            }]),
        });
        assert_eq!(
            serde_json::to_value(&generic).expect("serialize"),
            json!({
                "card": "generic",
                "title": "Open /a",
                "kind": "read",
                "rawInput": "/a",
                "locations": [{"path": "/a", "line": 3}],
            })
        );
        let terminal: ToolCallView = serde_json::from_value(json!({
            "card": "terminal",
            "title": "cargo test",
            "description": "Run tests",
            "cwd": "/workspace",
        }))
        .expect("terminal");
        assert!(matches!(terminal, ToolCallView::Terminal(_)));
    }

    #[test]
    fn completed_search_and_web_views_keep_both_discriminants() {
        let search = ToolResultView::Search(SearchResultView::Matches(SearchMatchesResultView {
            title: Some("Matches".to_owned()),
            files: vec![SearchFileMatches {
                path: "src/lib.rs".to_owned(),
                matches: vec![SearchLineMatch {
                    line_number: 7,
                    line: "needle".to_owned(),
                }],
            }],
            truncated: false,
            total: 1,
        }));
        assert_eq!(
            serde_json::to_value(search).expect("search"),
            json!({
                "card": "search",
                "shape": "matches",
                "title": "Matches",
                "files": [{
                    "path": "src/lib.rs",
                    "matches": [{"lineNumber": 7, "line": "needle"}],
                }],
                "truncated": false,
                "total": 1,
            })
        );
        let web = ToolResultView::Web(WebResultView::Fetch(WebFetchResultView {
            title: None,
            url: "https://example.com".to_owned(),
            status_code: 200,
            truncated: true,
        }));
        assert_eq!(
            serde_json::to_value(web).expect("web"),
            json!({
                "card": "web",
                "kind": "fetch",
                "url": "https://example.com",
                "statusCode": 200,
                "truncated": true,
            })
        );
    }

    #[test]
    fn every_pending_view_matches_the_source_wire_vocabulary() {
        let diff = FileDiff {
            path: "src/lib.rs".to_owned(),
            old_text: None,
            new_text: "fn main() {}\n".to_owned(),
        };
        let location = FileLocation {
            path: "src/lib.rs".to_owned(),
            line: None,
        };
        let pending = [
            ToolCallView::Terminal(TerminalCallView {
                title: "cargo test".to_owned(),
                description: None,
                cwd: Some("workspace".to_owned()),
            }),
            ToolCallView::Diff(DiffCallView {
                title: "Write src/lib.rs".to_owned(),
                diffs: vec![diff.clone()],
                locations: Some(vec![location]),
            }),
        ];
        assert_eq!(
            serde_json::to_value(pending).expect("pending views"),
            json!([
                {"card": "terminal", "title": "cargo test", "cwd": "workspace"},
                {
                    "card": "diff",
                    "title": "Write src/lib.rs",
                    "diffs": [{"path": "src/lib.rs", "oldText": null, "newText": "fn main() {}\n"}],
                    "locations": [{"path": "src/lib.rs"}],
                },
            ])
        );
    }

    #[test]
    fn every_completed_view_matches_the_source_wire_vocabulary() {
        let diff = FileDiff {
            path: "src/lib.rs".to_owned(),
            old_text: None,
            new_text: "fn main() {}\n".to_owned(),
        };
        let content = vec![ContentBlock::Text {
            text: "rendered".to_owned(),
        }];
        let completed = [
            ToolResultView::Generic(GenericResultView {
                title: None,
                content: Some(content.clone()),
            }),
            ToolResultView::Terminal(TerminalResultView {
                title: Some("Tests passed".to_owned()),
                output: Some("ok\n".to_owned()),
                exit_code: Some(0),
                signal: None,
            }),
            ToolResultView::Diff(DiffResultView {
                title: None,
                diffs: vec![diff],
            }),
            ToolResultView::Search(SearchResultView::Paths(SearchPathsResultView {
                title: None,
                paths: vec!["src/lib.rs".to_owned()],
                truncated: true,
                total: 2,
            })),
            ToolResultView::Read(ReadResultView {
                title: Some("Read src/lib.rs".to_owned()),
                path: "src/lib.rs".to_owned(),
                offset: 2,
                lines: vec![ReadFileLine {
                    number: 2,
                    text: "fn main() {}".to_owned(),
                }],
                total_lines: 3,
                lang: Some("rs".to_owned()),
                content: Some(content),
            }),
            ToolResultView::Web(WebResultView::Search(WebSearchResultView {
                title: None,
                sources: vec![WebSource {
                    url: "https://example.com/source".to_owned(),
                    title: Some("Source".to_owned()),
                    snippet: None,
                    published_at: Some("2026-08-17T00:00:00Z".to_owned()),
                }],
                answer: Some("answer".to_owned()),
                truncated: false,
            })),
        ];
        assert_eq!(
            serde_json::to_value(completed).expect("completed views"),
            json!([
                {"card": "generic", "content": [{"type": "text", "text": "rendered"}]},
                {"card": "terminal", "title": "Tests passed", "output": "ok\n", "exitCode": 0},
                {"card": "diff", "diffs": [{"path": "src/lib.rs", "oldText": null, "newText": "fn main() {}\n"}]},
                {"card": "search", "shape": "paths", "paths": ["src/lib.rs"], "truncated": true, "total": 2},
                {
                    "card": "read",
                    "title": "Read src/lib.rs",
                    "path": "src/lib.rs",
                    "offset": 2,
                    "lines": [{"number": 2, "text": "fn main() {}"}],
                    "totalLines": 3,
                    "lang": "rs",
                    "content": [{"type": "text", "text": "rendered"}],
                },
                {
                    "card": "web",
                    "kind": "search",
                    "sources": [{
                        "url": "https://example.com/source",
                        "title": "Source",
                        "publishedAt": "2026-08-17T00:00:00Z",
                    }],
                    "answer": "answer",
                    "truncated": false,
                },
            ])
        );
    }

    #[test]
    fn presentation_wire_unions_fail_closed_on_unknown_tags_and_fields() {
        for value in [
            json!({"card": "unknown", "title": "x"}),
            json!({"card": "generic", "title": "x", "extra": true}),
        ] {
            assert!(serde_json::from_value::<ToolCallView>(value).is_err());
        }
        for value in [
            json!({"card": "search", "shape": "unknown", "paths": [], "truncated": false, "total": 0}),
            json!({"card": "web", "kind": "unknown", "truncated": false}),
            json!({"card": "read", "path": "x", "offset": 1, "lines": [], "totalLines": 0, "extra": true}),
        ] {
            assert!(serde_json::from_value::<ToolResultView>(value).is_err());
        }

        let kinds = [
            ToolCallKind::Read,
            ToolCallKind::Edit,
            ToolCallKind::Delete,
            ToolCallKind::Move,
            ToolCallKind::Search,
            ToolCallKind::Execute,
            ToolCallKind::Fetch,
            ToolCallKind::Other,
        ];
        assert_eq!(
            serde_json::to_value(kinds).expect("tool call kinds"),
            json!([
                "read", "edit", "delete", "move", "search", "execute", "fetch", "other"
            ])
        );
    }

    #[test]
    fn durable_presenter_result_round_trips_content_failure_and_meta() {
        let result = ToolResult {
            content: vec![ContentBlock::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
            meta: Some(json!({"path": "/a"})),
        };
        let encoded = serde_json::to_value(&result).expect("encode");
        assert_eq!(
            serde_json::from_value::<ToolResult>(encoded).expect("decode"),
            result
        );
    }
}
