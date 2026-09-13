//! Portable composer-draft decoration derivation.

use std::collections::BTreeMap;

/// One input-reference trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReferenceTrigger {
    /// Slash skill/command-style reference.
    Slash,
    /// At-sign subagent-style reference.
    At,
}

impl ReferenceTrigger {
    /// Returns the source trigger character.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Slash => '/',
            Self::At => '@',
        }
    }
}

/// Stable identity for one inserted reference occurrence.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OccurrenceId(u64);

impl OccurrenceId {
    /// Brands one exact occurrence number.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the ordinary occurrence number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Claim-relevant input phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationPhase {
    /// A trigger owns the leading token.
    Claimed,
    /// The owned trigger is submitting.
    Submitting,
    /// Any phase without active claim decoration.
    Other,
}

/// Published claim currency needed by the decoration layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecorationClaim {
    /// Exact watched leading token.
    pub token: String,
    /// Optional ghost hint.
    pub hint: Option<String>,
}

/// One placeholder occurrence in draft order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecorationOccurrence {
    /// Stable occurrence identity.
    pub occurrence_id: OccurrenceId,
    /// UTF-16 placeholder offset.
    pub offset: u32,
    /// Rendered chip label.
    pub label: String,
    /// Owner-resolution failure bit.
    pub invalid: bool,
}

/// Minimal published input state consumed by decoration derivation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDecorationState {
    /// Complete draft text.
    pub draft: String,
    /// Current phase.
    pub phase: DecorationPhase,
    /// Optional leading claim.
    pub claim: Option<DecorationClaim>,
    /// Offset-sorted occurrence table.
    pub occurrences: Vec<DecorationOccurrence>,
}

/// One UTF-16 text range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenRange {
    /// Inclusive start.
    pub start: u32,
    /// Exclusive end.
    pub end: u32,
}

/// One chip render instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChipRender {
    /// Stable occurrence identity.
    pub occurrence_id: OccurrenceId,
    /// UTF-16 placeholder offset.
    pub offset: u32,
    /// Rendered label.
    pub label: String,
    /// Invalid-owner styling bit.
    pub invalid: bool,
}

/// One scan-derived plain reference range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRefRange {
    /// Inclusive UTF-16 start.
    pub start: u32,
    /// Exclusive UTF-16 end.
    pub end: u32,
    /// Matched trigger.
    pub trigger: ReferenceTrigger,
}

/// Complete mirror-layer decoration product.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftDecorations {
    /// Active leading claim range.
    pub token: Option<TokenRange>,
    /// Placeholder chips.
    pub chips: Vec<ChipRender>,
    /// Plain reference highlights.
    pub text_refs: Vec<TextRefRange>,
    /// Blank-argument ghost hint.
    pub hint: Option<String>,
}

/// Trigger-to-name hot lexicon.
pub type ReferenceLexicon = BTreeMap<ReferenceTrigger, Vec<String>>;

/// Scans plain `/name` and `@name` tokens against the current lexicon.
#[must_use]
pub fn scan_text_refs(draft: &str, lexicon: &ReferenceLexicon) -> Vec<TextRefRange> {
    if draft.is_empty() || lexicon.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut previous = None;
    for (byte_index, character) in draft.char_indices() {
        let trigger = match character {
            '/' => ReferenceTrigger::Slash,
            '@' => ReferenceTrigger::At,
            _ => {
                previous = Some(character);
                continue;
            }
        };
        if byte_index != 0 && !previous.is_some_and(is_javascript_whitespace) {
            previous = Some(character);
            continue;
        }
        let name_start = byte_index + character.len_utf8();
        let name_end = draft[name_start..]
            .char_indices()
            .take_while(|(_, value)| value.is_ascii_alphanumeric() || matches!(value, '_' | '-'))
            .last()
            .map_or(name_start, |(index, value)| {
                name_start + index + value.len_utf8()
            });
        let name = &draft[name_start..name_end];
        if !name.is_empty()
            && lexicon
                .get(&trigger)
                .is_some_and(|names| names.iter().any(|candidate| candidate == name))
        {
            let start = utf16_len(&draft[..byte_index]);
            let end = start.saturating_add(1).saturating_add(utf16_len(name));
            ranges.push(TextRefRange {
                start,
                end,
                trigger,
            });
        }
        previous = Some(character);
    }
    ranges
}

/// Derives the claim, chip, plain-reference, and ghost-hint decorations.
#[must_use]
pub fn derive_decorations(
    state: &InputDecorationState,
    lexicon: &ReferenceLexicon,
) -> DraftDecorations {
    let claim_active = matches!(
        state.phase,
        DecorationPhase::Claimed | DecorationPhase::Submitting
    ) && state
        .claim
        .as_ref()
        .is_some_and(|claim| state.draft.starts_with(&claim.token));
    let token = claim_active.then(|| TokenRange {
        start: 0,
        end: state
            .claim
            .as_ref()
            .map_or(0, |claim| utf16_len(&claim.token)),
    });
    let chips = state
        .occurrences
        .iter()
        .map(|occurrence| ChipRender {
            occurrence_id: occurrence.occurrence_id,
            offset: occurrence.offset,
            label: occurrence.label.clone(),
            invalid: occurrence.invalid,
        })
        .collect();
    let hint = if claim_active {
        state.claim.as_ref().and_then(|claim| {
            claim.hint.as_ref().and_then(|hint| {
                let arguments = &state.draft[claim.token.len()..];
                arguments
                    .chars()
                    .all(is_javascript_whitespace)
                    .then(|| hint.clone())
            })
        })
    } else {
        None
    };
    DraftDecorations {
        token,
        chips,
        text_refs: scan_text_refs(&state.draft, lexicon),
        hint,
    }
}

fn utf16_len(value: &str) -> u32 {
    u32::try_from(value.encode_utf16().count()).unwrap_or(u32::MAX)
}

const fn is_javascript_whitespace(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}
