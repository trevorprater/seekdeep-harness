//! Pure question presentation projections.

use std::sync::LazyLock;

use regex::Regex;
use seekdeep_user_questions_contract::{AskUserQuestionItem, AskUserQuestionOption};

static RECOMMENDED_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*(?:\((?:recommended|推荐)\)|（(?:recommended|推荐)）)\s*$")
        .expect("static recommendation suffix")
});

/// Display-only option label plus recommendation state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecommendedLabel {
    /// Label after removing one conventional suffix.
    pub label: String,
    /// Whether the suffix was present.
    pub recommended: bool,
}

/// Splits the conventional recommendation suffix without changing answer values.
#[must_use]
pub fn parse_recommended_label(label: &str) -> RecommendedLabel {
    let recommended = RECOMMENDED_SUFFIX.is_match(label);
    RecommendedLabel {
        label: if recommended {
            RECOMMENDED_SUFFIX.replace(label, "").into_owned()
        } else {
            label.to_owned()
        },
        recommended,
    }
}

/// One binary plan-review decision rendered outside the generic question flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanReview {
    /// Reviewed question identity echoed in the answer.
    pub id: String,
    /// Question text retained as the card's accessible name.
    pub question: String,
    /// Markdown body under review.
    pub plan: String,
    /// Asker-owned approval option.
    pub approve: AskUserQuestionOption,
    /// Asker-owned non-approval option, if present.
    pub decline: Option<AskUserQuestionOption>,
}

/// Narrows exactly one fully answerable binary plan-review request.
#[must_use]
pub fn plan_review_of(questions: &[AskUserQuestionItem]) -> Option<PlanReview> {
    let [question] = questions else {
        return None;
    };
    let intent = question.intent.as_ref()?;
    if intent.kind != "plan-review" || question.multi_select == Some(true) {
        return None;
    }
    let plan = question.detail.as_ref()?;
    let options = question.options.as_deref().unwrap_or_default();
    if options.len() > 2 {
        return None;
    }
    let approve = options
        .iter()
        .find(|option| option.label == intent.approve)?
        .clone();
    let decline = options
        .iter()
        .find(|option| option.label != intent.approve)
        .cloned();
    Some(PlanReview {
        id: question.id.clone(),
        question: question.question.clone(),
        plan: plan.clone(),
        approve,
        decline,
    })
}
