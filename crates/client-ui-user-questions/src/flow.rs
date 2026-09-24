//! Target-portable state machine for the generic question composer.

use seekdeep_user_questions_contract::{
    AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionItem,
};
use serde::{Deserialize, Serialize};

/// One local answer draft keyed positionally to the request batch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAnswer {
    /// Option labels selected verbatim.
    pub selected: Vec<String>,
    /// Free-text draft retained verbatim until submission.
    pub custom: String,
    /// Whether the user explicitly skipped this question.
    pub skipped: bool,
}

/// In-flight carrier operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionBusy {
    /// Whole-batch answer send.
    Answer,
    /// Whole-request cancellation send.
    Cancel,
}

/// Displayed feedback; dictionary keys remain live across locale changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum QuestionFeedback {
    /// At least one batch item remains incomplete.
    Incomplete,
    /// The current item has neither a selection nor custom text.
    Unanswered,
    /// Finished transport or receipt failure shown verbatim.
    Text(String),
}

impl QuestionFeedback {
    /// Locale dictionary key, absent for finished runtime text.
    #[must_use]
    pub const fn locale_key(&self) -> Option<&'static str> {
        match self {
            Self::Incomplete => Some("error.incomplete"),
            Self::Unanswered => Some("error.unanswered"),
            Self::Text(_) => None,
        }
    }
}

/// Result of a state transition that may emit the one whole answer batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionFlowEffect {
    /// Local state changed without a carrier send.
    None,
    /// Deliver this complete batch through the pending carrier.
    Answer(AskUserQuestionAnswer),
}

/// Generic question-flow state, independent of React and the browser executor.
#[derive(Clone, Debug, PartialEq)]
pub struct QuestionFlow {
    questions: Vec<AskUserQuestionItem>,
    index: usize,
    drafts: Vec<DraftAnswer>,
    busy: Option<QuestionBusy>,
    feedback: Option<QuestionFeedback>,
}

impl QuestionFlow {
    /// Creates fresh positional drafts for one validated non-empty request.
    ///
    /// # Panics
    ///
    /// Panics when the upstream-validated request contains no questions, matching the
    /// source composer's non-empty request invariant.
    #[must_use]
    pub fn new(questions: Vec<AskUserQuestionItem>) -> Self {
        assert!(
            !questions.is_empty(),
            "QuestionFlow requires at least one question"
        );
        let drafts = vec![DraftAnswer::default(); questions.len()];
        Self {
            questions,
            index: 0,
            drafts,
            busy: None,
            feedback: None,
        }
    }

    /// Current zero-based question position.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Immutable request batch.
    #[must_use]
    pub fn questions(&self) -> &[AskUserQuestionItem] {
        &self.questions
    }

    /// Current question.
    #[must_use]
    pub fn question(&self) -> &AskUserQuestionItem {
        &self.questions[self.index]
    }

    /// Every positional draft.
    #[must_use]
    pub fn drafts(&self) -> &[DraftAnswer] {
        &self.drafts
    }

    /// Current draft.
    #[must_use]
    pub fn draft(&self) -> &DraftAnswer {
        &self.drafts[self.index]
    }

    /// Current in-flight operation.
    #[must_use]
    pub const fn busy(&self) -> Option<QuestionBusy> {
        self.busy
    }

    /// Current validation or runtime feedback.
    #[must_use]
    pub fn feedback(&self) -> Option<&QuestionFeedback> {
        self.feedback.as_ref()
    }

    /// Whether navigation or answer controls are locked.
    #[must_use]
    pub const fn disabled(&self) -> bool {
        self.busy.is_some()
    }

    /// Whether the current draft carries an answer.
    #[must_use]
    pub fn current_answered(&self) -> bool {
        answered(self.draft())
    }

    /// Whether every draft is answered or explicitly skipped.
    #[must_use]
    pub fn all_completed(&self) -> bool {
        self.drafts.iter().all(completed)
    }

    /// Selects or toggles one exact option label and auto-advances single choice.
    pub fn choose(&mut self, label: &str) {
        let multi_select = self.question().multi_select == Some(true);
        let draft = &mut self.drafts[self.index];
        if multi_select {
            if let Some(position) = draft.selected.iter().position(|item| item == label) {
                draft.selected.remove(position);
            } else {
                draft.selected.push(label.to_owned());
            }
            draft.skipped = false;
        } else {
            *draft = DraftAnswer {
                selected: vec![label.to_owned()],
                custom: String::new(),
                skipped: false,
            };
        }
        self.feedback = None;
        if !multi_select && self.index + 1 < self.questions.len() {
            self.index += 1;
        }
    }

    /// Updates the current free-text answer, retaining multi-select labels only.
    pub fn set_custom(&mut self, value: impl Into<String>) {
        let multi_select = self.question().multi_select == Some(true);
        let draft = &mut self.drafts[self.index];
        if !multi_select {
            draft.selected.clear();
        }
        draft.custom = value.into();
        draft.skipped = false;
        self.feedback = None;
    }

    /// Moves to an earlier question when the source control would be enabled.
    pub fn previous(&mut self) {
        if self.index > 0 && self.busy.is_none() {
            self.index -= 1;
            self.feedback = None;
        }
    }

    /// Moves to a later question when the source control would be enabled.
    pub fn next(&mut self) {
        if self.index + 1 < self.questions.len() && self.busy.is_none() {
            self.index += 1;
            self.feedback = None;
        }
    }

    /// Continues after validation, or begins the one final answer send.
    pub fn continue_flow(&mut self) -> QuestionFlowEffect {
        if !answered(self.draft()) {
            self.feedback = Some(QuestionFeedback::Unanswered);
            return QuestionFlowEffect::None;
        }
        if self.index + 1 < self.questions.len() {
            self.index += 1;
            self.feedback = None;
            QuestionFlowEffect::None
        } else {
            self.submit()
        }
    }

    /// Handles Enter on an option: submit only when the entire batch is complete.
    pub fn enter_option(&mut self) -> QuestionFlowEffect {
        if self.all_completed() {
            self.submit()
        } else {
            QuestionFlowEffect::None
        }
    }

    /// Skips the current item, then advances or submits at the final item.
    pub fn skip(&mut self) -> QuestionFlowEffect {
        self.drafts[self.index] = DraftAnswer {
            skipped: true,
            ..DraftAnswer::default()
        };
        self.feedback = None;
        if self.index + 1 < self.questions.len() {
            self.index += 1;
            QuestionFlowEffect::None
        } else {
            self.submit()
        }
    }

    /// Begins cancellation and clears prior feedback.
    pub fn begin_cancel(&mut self) {
        self.busy = Some(QuestionBusy::Cancel);
        self.feedback = None;
    }

    /// Re-arms controls after a carrier failure and displays its finished text.
    pub fn fail(&mut self, message: impl Into<String>) {
        self.busy = None;
        self.feedback = Some(QuestionFeedback::Text(message.into()));
    }

    fn submit(&mut self) -> QuestionFlowEffect {
        if let Some(missing) = self.drafts.iter().position(|draft| !completed(draft)) {
            self.index = missing;
            self.feedback = Some(QuestionFeedback::Incomplete);
            return QuestionFlowEffect::None;
        }
        let answers = self
            .questions
            .iter()
            .zip(&self.drafts)
            .map(|(question, draft)| {
                if draft.skipped {
                    return AskUserQuestionAnswerItem {
                        id: question.id.clone(),
                        selected: Vec::new(),
                        custom: None,
                    };
                }
                let custom = draft.custom.trim();
                AskUserQuestionAnswerItem {
                    id: question.id.clone(),
                    selected: if custom.is_empty() || question.multi_select == Some(true) {
                        draft.selected.clone()
                    } else {
                        Vec::new()
                    },
                    custom: (!custom.is_empty()).then(|| custom.to_owned()),
                }
            })
            .collect();
        self.busy = Some(QuestionBusy::Answer);
        self.feedback = None;
        QuestionFlowEffect::Answer(AskUserQuestionAnswer { answers })
    }
}

fn answered(draft: &DraftAnswer) -> bool {
    !draft.selected.is_empty() || !draft.custom.trim().is_empty()
}

fn completed(draft: &DraftAnswer) -> bool {
    answered(draft) || draft.skipped
}
