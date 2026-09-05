//! Result-time contextual diff presentation for write and edit.

use seekdeep_tools::FileDiff;
use serde_json::Value;

/// Context lines shown on each side of an applied hunk.
pub const DIFF_CONTEXT: usize = 3;

/// The write/edit tools' private tool/result meta payload.
#[derive(Clone, Debug, PartialEq)]
pub struct FsDiffMeta {
    /// The applied contextual-diff hunks.
    pub diffs: Vec<FileDiff>,
}

/// Computes one diff per hunk between the before and after text, each carrying
/// the applied change plus context lines.
#[must_use]
pub fn compute_hunk_diffs(path: &str, before: &str, after: &str) -> Vec<FileDiff> {
    let old_lines = split_lines(before);
    let new_lines = split_lines(after);
    let script = diff_script(&old_lines, &new_lines);
    let hunks = group_hunks(&script);
    hunks
        .into_iter()
        .map(|hunk| build_diff(path, &old_lines, &new_lines, hunk))
        .collect()
}

/// Splits on newlines without treating a trailing newline as an empty line.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Equal,
    Delete,
    Insert,
}

/// Longest-common-subsequence edit script between two line lists.
fn diff_script(old: &[&str], new: &[&str]) -> Vec<Op> {
    let n = old.len();
    let m = new.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut script = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            script.push(Op::Equal);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            script.push(Op::Delete);
            i += 1;
        } else {
            script.push(Op::Insert);
            j += 1;
        }
    }
    while i < n {
        script.push(Op::Delete);
        i += 1;
    }
    while j < m {
        script.push(Op::Insert);
        j += 1;
    }
    script
}

/// A contiguous changed range plus its surrounding context, in old/new indices.
#[derive(Clone, Copy, Debug)]
struct Hunk {
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
}

/// Groups changed script positions into hunks joined within the context window.
fn group_hunks(script: &[Op]) -> Vec<Hunk> {
    let mut changed: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut i = 0;
    let mut j = 0;
    let mut index = 0;
    while index < script.len() {
        match script[index] {
            Op::Equal => {
                i += 1;
                j += 1;
                index += 1;
            }
            Op::Delete | Op::Insert => {
                let (mut di, mut dj) = (0, 0);
                while index < script.len() && script[index] != Op::Equal {
                    match script[index] {
                        Op::Delete => {
                            di += 1;
                            index += 1;
                        }
                        Op::Insert => {
                            dj += 1;
                            index += 1;
                        }
                        Op::Equal => unreachable!(),
                    }
                }
                changed.push((i, i + di, j, j + dj));
                i += di;
                j += dj;
            }
        }
    }

    let mut hunks: Vec<Hunk> = Vec::new();
    for (old_start, old_end, new_start, new_end) in changed {
        if let Some(last) = hunks.last_mut() {
            let gap = old_start.saturating_sub(last.old_end);
            if gap <= 2 * DIFF_CONTEXT {
                last.old_end = old_end;
                last.new_end = new_end;
                continue;
            }
        }
        hunks.push(Hunk {
            old_start,
            old_end,
            new_start,
            new_end,
        });
    }
    hunks
}

/// Builds one diff from a hunk, expanding by the context radius.
fn build_diff(path: &str, old_lines: &[&str], new_lines: &[&str], hunk: Hunk) -> FileDiff {
    let old_start = hunk.old_start.saturating_sub(DIFF_CONTEXT);
    let new_start = hunk.new_start.saturating_sub(DIFF_CONTEXT);
    let old_end = (hunk.old_end + DIFF_CONTEXT).min(old_lines.len());
    let new_end = (hunk.new_end + DIFF_CONTEXT).min(new_lines.len());

    let mut old_text: Vec<&str> = Vec::new();
    let mut new_text: Vec<&str> = Vec::new();
    let (mut oi, mut ni) = (old_start, new_start);
    while oi < old_end || ni < new_end {
        if oi < hunk.old_start && oi < old_end {
            old_text.push(old_lines[oi]);
            new_text.push(old_lines[oi]);
            oi += 1;
            ni += 1;
        } else if ni < hunk.new_start && ni < new_end {
            new_text.push(new_lines[ni]);
            old_text.push(new_lines[ni]);
            oi += 1;
            ni += 1;
        } else if oi < hunk.old_end && oi < old_end {
            old_text.push(old_lines[oi]);
            oi += 1;
        } else if ni < hunk.new_end && ni < new_end {
            new_text.push(new_lines[ni]);
            ni += 1;
        } else if oi < old_end {
            old_text.push(old_lines[oi]);
            oi += 1;
        } else {
            new_text.push(new_lines[ni]);
            ni += 1;
        }
    }

    FileDiff {
        path: path.to_owned(),
        old_text: (!old_text.is_empty()).then(|| old_text.join("\n")),
        new_text: new_text.join("\n"),
    }
}

/// Whether a value is a valid diff.
fn is_file_diff(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let path = object.get("path").is_some_and(Value::is_string);
    let old_text = object
        .get("oldText")
        .is_none_or(|value| value.is_null() || value.is_string());
    let new_text = object.get("newText").is_some_and(Value::is_string);
    path && old_text && new_text
}

/// Narrows opaque live or replayed result metadata to non-empty file diffs.
#[must_use]
pub fn diffs_from_meta(meta: &Value) -> Option<Vec<FileDiff>> {
    let object = meta.as_object()?;
    let diffs = object.get("diffs")?.as_array()?;
    if diffs.is_empty() || !diffs.iter().all(is_file_diff) {
        return None;
    }
    serde_json::from_value(Value::Array(diffs.clone())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_replacement() {
        let diffs = compute_hunk_diffs("a.txt", "one\ntwo\nthree\n", "one\nTWO\nthree\n");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "a.txt");
        assert_eq!(diffs[0].old_text.as_deref(), Some("one\ntwo\nthree"));
        assert_eq!(diffs[0].new_text, "one\nTWO\nthree");
    }

    #[test]
    fn pure_insertion_into_empty_file_has_null_old_text() {
        let diffs = compute_hunk_diffs("b.txt", "", "zero\none\n");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].old_text, None);
        assert_eq!(diffs[0].new_text, "zero\none");
    }

    #[test]
    fn identical_text_yields_no_diffs() {
        assert!(compute_hunk_diffs("c.txt", "same\n", "same\n").is_empty());
    }

    #[test]
    fn diffs_from_meta_accepts_and_rejects() {
        let diff = serde_json::json!({
            "path": "a.txt",
            "oldText": "one",
            "newText": "two"
        });
        let valid = serde_json::json!({"diffs": [diff]});
        assert!(diffs_from_meta(&valid).is_some());
        let invalid = serde_json::json!({"diffs": [{"path": 1}]});
        assert!(diffs_from_meta(&invalid).is_none());
        let absent = serde_json::json!({"other": true});
        assert!(diffs_from_meta(&absent).is_none());
    }
}
