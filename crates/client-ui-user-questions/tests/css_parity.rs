//! Exact source-CSS projection for compiled question surfaces.

use regex::Regex;
use seekdeep_client_ui_user_questions::{PLAN_REVIEW_STYLES, QUESTION_STYLES};

const QUESTION_SOURCE: &str = include_str!(
    "../../../packages/client/ui-user-questions/src/client/QuestionComposer.module.css"
);
const PLAN_SOURCE: &str = include_str!(
    "../../../packages/client/ui-user-questions/src/client/PlanReviewPanel.module.css"
);

fn namespace(source: &str, prefix: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, format!(".{prefix}$1"))
        .into_owned()
}

#[test]
fn compiled_question_styles_are_exact_namespaced_source_projections() {
    assert_eq!(
        QUESTION_STYLES,
        namespace(QUESTION_SOURCE, "seekdeep-question-")
    );
    assert_eq!(
        PLAN_REVIEW_STYLES,
        namespace(PLAN_SOURCE, "seekdeep-plan-review-")
    );
}
