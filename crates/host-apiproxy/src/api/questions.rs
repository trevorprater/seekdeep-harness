//! Batched user-question response wire contracts.

use seekdeep_core::session::SessionId;
use seekdeep_user_questions::{
    AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionIntent, AskUserQuestionItem,
    AskUserQuestionOption,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{
    rpc::ContractError,
    sessions::{
        optional_string, parse_array, require_array, require_bool, require_nonempty_string,
        require_object, require_string,
    },
};

/// Parses one strictly validated question item carried by a requested frame.
pub(super) fn parse_question_item(value: &Value) -> Result<AskUserQuestionItem, ContractError> {
    let object = require_object(value, "$")?;
    let id = require_string(object, "id", "$.id", false)?.to_owned();
    let question = require_string(object, "question", "$.question", false)?.to_owned();
    optional_string(object, "header", "$.header", false)?;
    optional_string(object, "detail", "$.detail", false)?;
    let options = object
        .get("options")
        .map(|value| {
            let values = value
                .as_array()
                .ok_or_else(|| ContractError::new("$.options", "expected array"))?;
            parse_array(
                values,
                |value| {
                    let option = require_object(value, "$")?;
                    let label = require_string(option, "label", "$.label", false)?.to_owned();
                    optional_string(option, "description", "$.description", false)?;
                    Ok(AskUserQuestionOption {
                        label,
                        description: option
                            .get("description")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    })
                },
                "$.options",
            )
        })
        .transpose()?;
    let multi_select = object
        .get("multiSelect")
        .map(|_| require_bool(object, "multiSelect", "$.multiSelect"))
        .transpose()?;
    let intent = object
        .get("intent")
        .map(|value| {
            let intent = require_object(value, "$.intent")?;
            if require_string(intent, "kind", "$.intent.kind", false)? != "plan-review" {
                return Err(ContractError::new(
                    "$.intent.kind",
                    "unknown question presentation intent",
                ));
            }
            Ok(AskUserQuestionIntent {
                kind: "plan-review".to_owned(),
                approve: require_string(intent, "approve", "$.intent.approve", false)?.to_owned(),
                extra: Map::new(),
            })
        })
        .transpose()?;
    Ok(AskUserQuestionItem {
        id,
        question,
        detail: object
            .get("detail")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        header: object
            .get("header")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        options,
        multi_select,
        intent,
    })
}

/// Parses a complete batched answer.
///
/// # Errors
///
/// Returns an error unless every answer has string id, string selections, and optional string custom text.
pub fn parse_question_answer(value: &Value) -> Result<AskUserQuestionAnswer, ContractError> {
    let object = require_object(value, "$")?;
    let answers = parse_array(
        require_array(object, "answers", "$.answers")?,
        |value| {
            let answer = require_object(value, "$")?;
            let id = require_string(answer, "id", "$.id", false)?.to_owned();
            let selected = require_array(answer, "selected", "$.selected")?;
            if !selected.iter().all(Value::is_string) {
                return Err(ContractError::new("$.selected", "expected string array"));
            }
            optional_string(answer, "custom", "$.custom", false)?;
            Ok(AskUserQuestionAnswerItem {
                id,
                selected: selected
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect(),
                custom: answer
                    .get("custom")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
        },
        "$.answers",
    )?;
    Ok(AskUserQuestionAnswer { answers })
}

/// Result value of a batched question Client response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionResponsePayload {
    /// Owning Session.
    pub session_id: SessionId,
    /// Whole-batch answer.
    pub answer: AskUserQuestionAnswer,
}

impl QuestionResponsePayload {
    /// Parses a question response payload.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty Session id or malformed batched answer.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            session_id: SessionId::new(require_nonempty_string(
                object,
                "sessionId",
                "$.sessionId",
            )?),
            answer: parse_question_answer(
                object.get("answer").ok_or_else(|| {
                    ContractError::new("$.answer", "required property is missing")
                })?,
            )?,
        })
    }
}
