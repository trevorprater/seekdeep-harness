//! Model option flattening, effort selection, defaults, and locale parity.

use seekdeep_client_ui_model_selection::{
    MODEL_LOCALES, MODEL_NS, ModelCatalogFailure, ModelDirectoryState, ModelDirectoryStatus,
    ModelEntry, ModelId, ModelProviderGroup, ModelProviderId, ModelReasoning, ModelSelection,
    ReasoningEffort, ReasoningEffortId, SessionModels, options_of, row_id, selection_of,
};

fn provider(value: &str) -> ModelProviderId {
    ModelProviderId::new(value)
}
fn model(value: &str) -> ModelId {
    ModelId::new(value)
}
fn effort(value: &str) -> ReasoningEffortId {
    ReasoningEffortId::new(value)
}

fn directory() -> SessionModels {
    SessionModels {
        current: ModelSelection {
            provider: provider("deepseek-official"),
            model: model("deepseek-v4-flash"),
            reasoning_effort: Some(effort("max")),
        },
        routable: true,
        groups: vec![ModelProviderGroup {
            id: provider("deepseek-official"),
            name: "DeepSeek".to_owned(),
            models: vec![
                ModelEntry {
                    id: model("deepseek-v4-flash"),
                    name: "DeepSeek-V4-Flash".to_owned(),
                    description: Some("Fast".to_owned()),
                    reasoning: Some(ModelReasoning {
                        efforts: vec![ReasoningEffort {
                            id: effort("high"),
                            name: "High".to_owned(),
                            description: None,
                        }],
                        default_effort: Some(effort("high")),
                    }),
                },
                ModelEntry {
                    id: model("deepseek-v4-pro"),
                    name: "DeepSeek-V4-Pro".to_owned(),
                    description: None,
                    reasoning: None,
                },
            ],
        }],
        failures: vec![ModelCatalogFailure {
            id: provider("broken"),
            name: "Broken Provider".to_owned(),
            message: "offline".to_owned(),
        }],
    }
}

#[test]
fn options_and_opaque_selection_rules_match_the_source() {
    let directory = directory();
    let rows = options_of(&directory, |message| format!("Catalog failed: {message}"));
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].id, "deepseek-official/deepseek-v4-flash");
    assert_eq!(rows[0].detail.as_deref(), Some("DeepSeek · Fast"));
    assert_eq!(rows[0].active, Some(true));
    assert_eq!(rows[1].detail.as_deref(), Some("DeepSeek"));
    assert_eq!(rows[2].id, "failure/broken");
    assert_eq!(rows[2].detail.as_deref(), Some("Catalog failed: offline"));

    let state = ModelDirectoryState {
        current: Some(directory.current.clone()),
        routable: Some(true),
        groups: directory.groups.clone(),
        failures: directory.failures.clone(),
        status: ModelDirectoryStatus::Ready,
        error: None,
    };
    let same = selection_of(&state, "deepseek-official/deepseek-v4-flash").unwrap();
    assert_eq!(same.reasoning_effort.unwrap().as_str(), "max");
    assert_eq!(
        selection_of(&state, "deepseek-official/deepseek-v4-pro")
            .unwrap()
            .reasoning_effort,
        None
    );
    assert_eq!(selection_of(&state, "failure/broken"), None);
    assert_eq!(selection_of(&state, "stale"), None);

    let other_route = ModelDirectoryState {
        current: Some(ModelSelection {
            provider: provider("other"),
            model: model("other"),
            reasoning_effort: None,
        }),
        ..state
    };
    assert_eq!(
        selection_of(&other_route, "deepseek-official/deepseek-v4-flash")
            .unwrap()
            .reasoning_effort
            .unwrap()
            .as_str(),
        "high"
    );
    assert_eq!(
        row_id(&provider("provider/with/slash"), &model("model")),
        "provider/with/slash/model"
    );
}

#[test]
fn directory_defaults_and_locale_copy_are_exact() {
    let state = ModelDirectoryState::default();
    assert_eq!(state.current, None);
    assert_eq!(state.routable, None);
    assert!(state.groups.is_empty());
    assert_eq!(state.status, ModelDirectoryStatus::Idle);
    assert_eq!(MODEL_NS, "model");
    assert_eq!(MODEL_LOCALES.len(), 17);
    assert_eq!(
        MODEL_LOCALES[5],
        (
            "trigger.ariaEffort",
            "选择模型，当前 {model}，推理等级 {effort}",
            "Select model, current {model}, reasoning effort {effort}"
        )
    );
    assert_eq!(
        MODEL_LOCALES[16],
        (
            "empty.efforts",
            "当前模型未提供推理等级。",
            "This model provides no reasoning effort levels."
        )
    );
}
