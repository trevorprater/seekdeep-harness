//! Bounded model-facing output with exact omission metadata.

use thiserror::Error;

/// How much otherwise-available content was omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Omitted {
    /// Nothing was omitted.
    None,
    /// The exact number of omitted units is known.
    Exact(usize),
    /// Content was omitted, but its amount is not known.
    Unknown,
}

/// Result returned after offering one item or byte chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushDecision {
    /// Whether this entire item or all bytes in this chunk were retained.
    pub kept: bool,
    /// Whether anything observed so far has been omitted due to the budget.
    pub truncated: bool,
}

/// Final snapshot of an item retainer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedItems<T> {
    /// Retained items in input order.
    pub items: Vec<T>,
    /// Whether the retainer omitted any observed item.
    pub truncated: bool,
    /// Number of observed items.
    pub seen: usize,
    /// Number of retained items.
    pub kept: usize,
    /// Exact omission metadata.
    pub omitted: Omitted,
}

/// Final snapshot of a text retainer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedText {
    /// Retained text, decoded as non-fatal UTF-8.
    pub text: String,
    /// Whether any observed byte was omitted.
    pub truncated: bool,
    /// Exact byte omission metadata.
    pub omitted_bytes: Omitted,
}

/// Invalid JavaScript-compatible numeric budget.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{field} must be a non-negative integer")]
pub struct InvalidBudget {
    field: &'static str,
}

fn checked_budget(value: f64, field: &'static str) -> Result<usize, InvalidBudget> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
        return Err(InvalidBudget { field });
    }
    // Rust float-to-integer conversion saturates. A valid JavaScript integer
    // larger than this process can ever observe becomes `usize::MAX`, which is
    // behaviorally indistinguishable for every realizable input stream.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value as usize)
}

/// Keeps the first bounded number of logical items while counting all input.
#[derive(Clone, Debug)]
pub struct ItemRetainer<T> {
    max_items: usize,
    items: Vec<T>,
    seen: usize,
    omitted_count: usize,
}

impl<T> ItemRetainer<T> {
    /// Constructs a retainer from the source package's number-shaped budget.
    ///
    /// # Errors
    ///
    /// Rejects negative, fractional, or non-finite values with the source
    /// contract's field-specific message.
    pub fn try_new(max_items: f64) -> Result<Self, InvalidBudget> {
        let max_items = checked_budget(max_items, "maxItems")?;
        Ok(Self::new(max_items))
    }

    /// Constructs a retainer from an already validated Rust item count.
    #[must_use]
    pub fn new(max_items: usize) -> Self {
        Self {
            max_items,
            items: Vec::new(),
            seen: 0,
            omitted_count: 0,
        }
    }

    /// Offers one prepared logical item.
    pub fn push(&mut self, item: T) -> PushDecision {
        self.seen += 1;
        if self.items.len() < self.max_items {
            self.items.push(item);
            PushDecision {
                kept: true,
                truncated: false,
            }
        } else {
            self.omitted_count += 1;
            PushDecision {
                kept: false,
                truncated: true,
            }
        }
    }

    /// Consumes the retainer and returns its final snapshot.
    pub fn finish(self) -> RetainedItems<T> {
        let truncated = self.omitted_count > 0;
        let kept = self.items.len();
        RetainedItems {
            items: self.items,
            truncated,
            seen: self.seen,
            kept,
            omitted: if truncated {
                Omitted::Exact(self.omitted_count)
            } else {
                Omitted::None
            },
        }
    }
}

/// Byte retention strategy for text streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRetentionStrategy {
    /// Keep the first `max_bytes` bytes.
    Head {
        /// Prefix byte budget.
        max_bytes: usize,
    },
    /// Keep the final `max_bytes` bytes.
    Tail {
        /// Suffix byte budget.
        max_bytes: usize,
    },
    /// Keep a stable prefix and suffix, omitting the middle.
    HeadTail {
        /// Prefix byte budget.
        head_bytes: usize,
        /// Suffix byte budget.
        tail_bytes: usize,
    },
}

impl TextRetentionStrategy {
    /// Validates and constructs a source-compatible head strategy.
    ///
    /// # Errors
    ///
    /// Rejects a negative, fractional, or non-finite byte count.
    pub fn try_head(max_bytes: f64) -> Result<Self, InvalidBudget> {
        Ok(Self::Head {
            max_bytes: checked_budget(max_bytes, "maxBytes")?,
        })
    }

    /// Validates and constructs a source-compatible tail strategy.
    ///
    /// # Errors
    ///
    /// Rejects a negative, fractional, or non-finite byte count.
    pub fn try_tail(max_bytes: f64) -> Result<Self, InvalidBudget> {
        Ok(Self::Tail {
            max_bytes: checked_budget(max_bytes, "maxBytes")?,
        })
    }

    /// Validates and constructs a source-compatible head-and-tail strategy.
    ///
    /// # Errors
    ///
    /// Rejects either negative, fractional, or non-finite byte count.
    pub fn try_head_tail(head_bytes: f64, tail_bytes: f64) -> Result<Self, InvalidBudget> {
        Ok(Self::HeadTail {
            head_bytes: checked_budget(head_bytes, "headBytes")?,
            tail_bytes: checked_budget(tail_bytes, "tailBytes")?,
        })
    }
}

/// Bounded byte-oriented text accumulator.
#[derive(Clone, Debug)]
pub struct TextRetainer {
    prefix_cap: usize,
    suffix_cap: usize,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    total: usize,
}

impl TextRetainer {
    /// Constructs a text retainer from a validated strategy.
    #[must_use]
    pub fn new(strategy: TextRetentionStrategy) -> Self {
        let (prefix_cap, suffix_cap) = match strategy {
            TextRetentionStrategy::Head { max_bytes } => (max_bytes, 0),
            TextRetentionStrategy::Tail { max_bytes } => (0, max_bytes),
            TextRetentionStrategy::HeadTail {
                head_bytes,
                tail_bytes,
            } => (head_bytes, tail_bytes),
        };
        Self {
            prefix_cap,
            suffix_cap,
            prefix: Vec::new(),
            suffix: Vec::new(),
            total: 0,
        }
    }

    /// Offers a UTF-8 string chunk.
    pub fn push_str(&mut self, chunk: &str) -> PushDecision {
        self.push(chunk.as_bytes())
    }

    /// Offers a raw byte chunk.
    ///
    /// # Panics
    ///
    /// Panics only if the cumulative observed byte count exceeds `usize::MAX`,
    /// which cannot be represented by either the retained result or this
    /// process's address space.
    pub fn push(&mut self, bytes: &[u8]) -> PushDecision {
        let before = self.total;
        self.total = self
            .total
            .checked_add(bytes.len())
            .expect("text retention byte count overflow");

        let take = self
            .prefix_cap
            .saturating_sub(self.prefix.len())
            .min(bytes.len());
        self.prefix.extend_from_slice(&bytes[..take]);

        if self.suffix_cap > 0 {
            if bytes.len() >= self.suffix_cap {
                self.suffix.clear();
                self.suffix
                    .extend_from_slice(&bytes[bytes.len() - self.suffix_cap..]);
            } else {
                let excess = self
                    .suffix
                    .len()
                    .saturating_add(bytes.len())
                    .saturating_sub(self.suffix_cap);
                if excess > 0 {
                    self.suffix.drain(..excess);
                }
                self.suffix.extend_from_slice(bytes);
            }
        }

        let omitted_before = self.omitted_at(before);
        let omitted_now = self.omitted_at(self.total);
        PushDecision {
            kept: omitted_now == omitted_before,
            truncated: omitted_now > 0,
        }
    }

    fn omitted_at(&self, total: usize) -> usize {
        let prefix_len = total.min(self.prefix_cap);
        let suffix_len = total.saturating_sub(prefix_len).min(self.suffix_cap);
        total - prefix_len - suffix_len
    }

    /// Consumes the retainer, trims true UTF-8 cut boundaries, and returns text.
    pub fn finish(self) -> RetainedText {
        let prefix_len = self.total.min(self.prefix_cap);
        let suffix_len = self.total.saturating_sub(prefix_len).min(self.suffix_cap);
        debug_assert_eq!(self.prefix.len(), prefix_len);
        let suffix_start = self.suffix.len() - suffix_len;
        let suffix = &self.suffix[suffix_start..];
        let budget_omitted = self.omitted_at(self.total);

        let (kept_prefix, kept_suffix) = if budget_omitted > 0 {
            (
                trim_trailing_partial_utf8(&self.prefix),
                trim_leading_continuation_utf8(suffix),
            )
        } else {
            (self.prefix.as_slice(), suffix)
        };

        let mut retained = Vec::with_capacity(kept_prefix.len() + kept_suffix.len());
        retained.extend_from_slice(kept_prefix);
        retained.extend_from_slice(kept_suffix);
        let omitted_count = self.total - kept_prefix.len() - kept_suffix.len();
        RetainedText {
            text: String::from_utf8_lossy(&retained).into_owned(),
            truncated: omitted_count > 0,
            omitted_bytes: if omitted_count > 0 {
                Omitted::Exact(omitted_count)
            } else {
                Omitted::None
            },
        }
    }
}

fn trim_trailing_partial_utf8(bytes: &[u8]) -> &[u8] {
    let Some(mut index) = bytes.len().checked_sub(1) else {
        return bytes;
    };
    while bytes[index] & 0xc0 == 0x80 && bytes.len() - index <= 3 {
        let Some(previous) = index.checked_sub(1) else {
            return bytes;
        };
        index = previous;
    }
    let lead = bytes[index];
    let expected = if lead < 0x80 {
        1
    } else if lead < 0xe0 {
        2
    } else if lead < 0xf0 {
        3
    } else if lead < 0xf8 {
        4
    } else {
        0
    };
    if expected == 0 || bytes.len() - index >= expected {
        bytes
    } else {
        &bytes[..index]
    }
}

fn trim_leading_continuation_utf8(bytes: &[u8]) -> &[u8] {
    let first_lead = bytes
        .iter()
        .position(|byte| byte & 0xc0 != 0x80)
        .unwrap_or(bytes.len());
    &bytes[first_lead..]
}

/// Retention strategy label used in notices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionStrategyKind {
    /// Prefix retention.
    Head,
    /// Suffix retention.
    Tail,
    /// Prefix and suffix retention.
    HeadTail,
}

/// Omission unit label used in notices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionUnit {
    /// Logical items.
    Items,
    /// Bytes.
    Bytes,
    /// Characters.
    Chars,
    /// Lines.
    Lines,
}

impl RetentionUnit {
    fn as_str(self) -> &'static str {
        match self {
            Self::Items => "items",
            Self::Bytes => "bytes",
            Self::Chars => "chars",
            Self::Lines => "lines",
        }
    }
}

/// Scalar or split retention limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionLimit {
    /// One strategy-wide limit.
    Single(usize),
    /// Separate head and tail limits.
    HeadTail {
        /// Head limit.
        head: usize,
        /// Tail limit.
        tail: usize,
    },
}

/// Neutral mechanical facts for a model-facing retention notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionNotice {
    /// Tool or output scope label.
    pub scope: String,
    /// Applied strategy.
    pub strategy: RetentionStrategyKind,
    /// Count unit.
    pub unit: RetentionUnit,
    /// Applied limit.
    pub limit: RetentionLimit,
    /// Number of retained units.
    pub kept: usize,
    /// Omission metadata.
    pub omitted: Omitted,
}

/// Formats the standardized omission clause without false precision.
#[must_use]
pub fn describe_omitted(omitted: Omitted, unit: RetentionUnit) -> String {
    match omitted {
        Omitted::None => String::new(),
        Omitted::Exact(count) => format!("Omitted {count} {}.", unit.as_str()),
        Omitted::Unknown => format!("More {} were omitted.", unit.as_str()),
    }
}

/// Combines the standardized omission clause with tool-owned recovery text.
pub fn format_retention_notice(
    notice: &RetentionNotice,
    recovery: impl FnOnce(&RetentionNotice) -> String,
) -> String {
    let omission = describe_omitted(notice.omitted, notice.unit);
    let recovery = recovery(notice);
    match (omission.is_empty(), recovery.is_empty()) {
        (true, _) => recovery,
        (_, true) => omission,
        (false, false) => format!("{omission} {recovery}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(count: usize) -> Omitted {
        Omitted::Exact(count)
    }

    #[test]
    fn item_retainer_keeps_head_and_counts_all_omissions() {
        let mut retainer = ItemRetainer::new(2);
        assert_eq!(
            retainer.push("a"),
            PushDecision {
                kept: true,
                truncated: false
            }
        );
        assert_eq!(
            retainer.push("b"),
            PushDecision {
                kept: true,
                truncated: false
            }
        );
        assert_eq!(
            retainer.push("c"),
            PushDecision {
                kept: false,
                truncated: true
            }
        );
        let result = retainer.finish();
        assert_eq!(result.items, ["a", "b"]);
        assert_eq!(result.seen, 3);
        assert_eq!(result.kept, 2);
        assert!(result.truncated);
        assert_eq!(result.omitted, exact(1));
    }

    #[test]
    fn item_retainer_zero_budget_and_numeric_validation() {
        let mut retainer = ItemRetainer::new(0);
        assert_eq!(
            retainer.push("a"),
            PushDecision {
                kept: false,
                truncated: true
            }
        );
        assert_eq!(retainer.finish().omitted, exact(1));
        assert_eq!(
            ItemRetainer::<()>::try_new(-1.0).unwrap_err().to_string(),
            "maxItems must be a non-negative integer"
        );
        assert!(ItemRetainer::<()>::try_new(1.5).is_err());
        assert!(ItemRetainer::<()>::try_new(f64::NAN).is_err());
        let mut enormous = ItemRetainer::try_new(1.0e100).unwrap();
        assert!(enormous.push("still practical").kept);
    }

    #[test]
    fn head_retention_is_exact_and_marks_partial_chunks() {
        let mut retainer = TextRetainer::new(TextRetentionStrategy::Head { max_bytes: 5 });
        assert_eq!(
            retainer.push_str("abc"),
            PushDecision {
                kept: true,
                truncated: false
            }
        );
        assert_eq!(
            retainer.push_str("de"),
            PushDecision {
                kept: true,
                truncated: false
            }
        );
        assert_eq!(
            retainer.push_str("fgh"),
            PushDecision {
                kept: false,
                truncated: true
            }
        );
        let result = retainer.finish();
        assert_eq!(result.text, "abcde");
        assert_eq!(result.omitted_bytes, exact(3));

        let mut partial = TextRetainer::new(TextRetentionStrategy::Head { max_bytes: 4 });
        partial.push_str("ab");
        assert_eq!(
            partial.push_str("cde"),
            PushDecision {
                kept: false,
                truncated: true
            }
        );
        assert_eq!(partial.finish().text, "abcd");
    }

    #[test]
    fn tail_retention_rolls_to_the_final_window() {
        let mut retainer = TextRetainer::new(TextRetentionStrategy::Tail { max_bytes: 4 });
        assert_eq!(
            retainer.push_str("hello"),
            PushDecision {
                kept: false,
                truncated: true
            }
        );
        retainer.push_str("world");
        let result = retainer.finish();
        assert_eq!(result.text, "orld");
        assert_eq!(result.omitted_bytes, exact(6));

        let mut chunks = TextRetainer::new(TextRetentionStrategy::Tail { max_bytes: 3 });
        for chunk in ["11", "22", "33", "44"] {
            chunks.push_str(chunk);
        }
        assert_eq!(chunks.finish().text, "344");
    }

    #[test]
    fn head_tail_does_not_double_count_or_break_an_artificial_split() {
        let mut exact_fit = TextRetainer::new(TextRetentionStrategy::HeadTail {
            head_bytes: 3,
            tail_bytes: 3,
        });
        exact_fit.push_str("abcdef");
        assert_eq!(
            exact_fit.finish(),
            RetainedText {
                text: "abcdef".into(),
                truncated: false,
                omitted_bytes: Omitted::None
            }
        );

        let mut split_codepoint = TextRetainer::new(TextRetentionStrategy::HeadTail {
            head_bytes: 1,
            tail_bytes: 3,
        });
        split_codepoint.push_str("éab");
        assert_eq!(split_codepoint.finish().text, "éab");

        let mut omitted = TextRetainer::new(TextRetentionStrategy::HeadTail {
            head_bytes: 3,
            tail_bytes: 3,
        });
        omitted.push_str("abcdefghij");
        assert_eq!(
            omitted.finish(),
            RetainedText {
                text: "abchij".into(),
                truncated: true,
                omitted_bytes: exact(4)
            }
        );
    }

    #[test]
    fn real_utf8_cuts_trim_partial_codepoints_and_count_trimmed_bytes() {
        let mut head = TextRetainer::new(TextRetentionStrategy::Head { max_bytes: 2 });
        head.push_str("a€b");
        assert_eq!(
            head.finish(),
            RetainedText {
                text: "a".into(),
                truncated: true,
                omitted_bytes: exact(4)
            }
        );

        let mut tail = TextRetainer::new(TextRetentionStrategy::Tail { max_bytes: 2 });
        tail.push_str("a€b");
        assert_eq!(
            tail.finish(),
            RetainedText {
                text: "b".into(),
                truncated: true,
                omitted_bytes: exact(4)
            }
        );

        let mut both = TextRetainer::new(TextRetentionStrategy::HeadTail {
            head_bytes: 2,
            tail_bytes: 2,
        });
        both.push_str("a€€b");
        assert_eq!(
            both.finish(),
            RetainedText {
                text: "ab".into(),
                truncated: true,
                omitted_bytes: exact(6)
            }
        );
    }

    #[test]
    fn malformed_interior_bytes_are_left_for_lossy_decoding() {
        let mut continuations = TextRetainer::new(TextRetentionStrategy::Head { max_bytes: 2 });
        continuations.push(&[0x80, 0x80, b'z']);
        assert_eq!(continuations.finish().omitted_bytes, exact(1));

        let mut invalid_lead = TextRetainer::new(TextRetentionStrategy::Head { max_bytes: 1 });
        invalid_lead.push(&[0xf8, b'a']);
        assert_eq!(invalid_lead.finish().omitted_bytes, exact(1));
    }

    #[test]
    fn zero_budgets_and_strategy_validation_match_source_contract() {
        let mut zero = TextRetainer::new(TextRetentionStrategy::Head { max_bytes: 0 });
        assert_eq!(
            zero.push_str("x"),
            PushDecision {
                kept: false,
                truncated: true
            }
        );
        assert_eq!(zero.finish().omitted_bytes, exact(1));

        assert_eq!(
            TextRetentionStrategy::try_head(-1.0)
                .unwrap_err()
                .to_string(),
            "maxBytes must be a non-negative integer"
        );
        assert!(TextRetentionStrategy::try_tail(2.5).is_err());
        assert_eq!(
            TextRetentionStrategy::try_head_tail(-1.0, 2.0)
                .unwrap_err()
                .to_string(),
            "headBytes must be a non-negative integer"
        );
        assert_eq!(
            TextRetentionStrategy::try_head_tail(2.0, 1.1)
                .unwrap_err()
                .to_string(),
            "tailBytes must be a non-negative integer"
        );
        let mut enormous = TextRetainer::new(TextRetentionStrategy::try_head(1.0e100).unwrap());
        assert!(enormous.push_str("still practical").kept);
    }

    #[test]
    fn notice_formatting_avoids_false_precision_and_empty_halves() {
        assert_eq!(
            describe_omitted(exact(3), RetentionUnit::Items),
            "Omitted 3 items."
        );
        assert_eq!(
            describe_omitted(Omitted::Unknown, RetentionUnit::Lines),
            "More lines were omitted."
        );
        assert_eq!(describe_omitted(Omitted::None, RetentionUnit::Chars), "");

        let notice = RetentionNotice {
            scope: "grep".into(),
            strategy: RetentionStrategyKind::Head,
            unit: RetentionUnit::Items,
            limit: RetentionLimit::Single(100),
            kept: 100,
            omitted: exact(25),
        };
        assert_eq!(
            format_retention_notice(&notice, |value| format!(
                "Results capped at {}. Narrow the pattern, path, or include to see more.",
                value.kept
            )),
            "Omitted 25 items. Results capped at 100. Narrow the pattern, path, or include to see more."
        );
        assert_eq!(
            format_retention_notice(
                &RetentionNotice {
                    omitted: Omitted::None,
                    ..notice.clone()
                },
                |_| "Recovery text.".into()
            ),
            "Recovery text."
        );
        assert_eq!(
            format_retention_notice(
                &RetentionNotice {
                    omitted: exact(2),
                    ..notice
                },
                |_| String::new()
            ),
            "Omitted 2 items."
        );
    }
}
