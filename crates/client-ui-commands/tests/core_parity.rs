//! Popup filter, fuzzy ranking, command parsing, contract, and locale parity.

use std::borrow::Cow;

use seekdeep_client_ui_commands::{
    COMMAND_LOCALES, COMMAND_NS, PopupState, PopupStatus, SelectConfirmation, SelectOption,
    filter_options, fuzzy_candidates, fuzzy_score, submitted_command_name,
};
use seekdeep_client_ui_input_trigger::InputTriggerCandidate;

fn option(id: &str, label: &str, detail: Option<&str>) -> SelectOption {
    SelectOption {
        id: id.to_owned(),
        label: label.to_owned(),
        detail: detail.map(ToOwned::to_owned),
        active: None,
        confirmation: None,
    }
}

#[test]
fn popup_filter_is_case_insensitive_over_label_and_detail_with_blank_identity() {
    let options = vec![
        option("dark", "Dark", None),
        SelectOption {
            active: Some(true),
            ..option("light", "Light", None)
        },
        option("sepia", "Sepia", Some("warm")),
    ];
    assert!(matches!(filter_options(&options, ""), Cow::Borrowed(_)));
    assert!(matches!(
        filter_options(&options, "  \u{feff}"),
        Cow::Borrowed(_)
    ));
    assert_eq!(
        filter_options(&options, "DARK")
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>(),
        ["dark"]
    );
    assert_eq!(
        filter_options(&options, "warm")
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>(),
        ["sepia"]
    );
    assert!(filter_options(&options, "nope").is_empty());
}

fn candidates(names: &[&str]) -> Vec<InputTriggerCandidate> {
    names
        .iter()
        .map(|name| InputTriggerCandidate::named(*name))
        .collect()
}

fn ranked(names: &[&str], query: &str) -> Vec<String> {
    let candidates = candidates(names);
    fuzzy_candidates(&candidates, query)
        .into_iter()
        .map(|candidate| candidate.name.clone())
        .collect()
}

#[test]
fn fuzzy_ranking_preserves_prefix_boundary_adjacency_gap_and_source_order() {
    let names = [
        "q-xylophone",
        "qx-long",
        "fabulous",
        "foo-bar",
        "zuv",
        "zu1v",
        "yu1v",
        "zu12v",
    ];
    assert_eq!(ranked(&names, "QX"), ["qx-long", "q-xylophone"]);
    assert_eq!(ranked(&names, "fb"), ["foo-bar", "fabulous"]);
    assert_eq!(ranked(&names, "uv"), ["zuv", "zu1v", "yu1v", "zu12v"]);
    assert!(ranked(&names, "zzz").is_empty());
    assert!(ranked(&names, "query-longer-than-every-name").is_empty());
    assert_eq!(fuzzy_score("anything", ""), Some(0));
    assert_eq!(ranked(&["same", "same"], "same"), ["same", "same"]);
}

#[test]
fn command_name_contract_defaults_and_locales_are_exact() {
    assert_eq!(submitted_command_name(" /goal ship it "), "goal");
    assert_eq!(submitted_command_name("\u{feff}/theme"), "theme");
    assert_eq!(submitted_command_name("/"), "");

    let state = PopupState::default();
    assert!(!state.open);
    assert_eq!(state.command, None);
    assert_eq!(state.status, PopupStatus::Pending);
    assert!(state.options.is_empty());
    assert_eq!(state.active, 0);
    assert!(!state.submitting);
    assert_eq!(state.confirming, None);
    assert!(!state.acknowledged);
    assert_eq!(state.error, None);

    let confirmation = SelectConfirmation {
        title: "Enable Full access?".to_owned(),
        description: "Sensitive operations.".to_owned(),
        acknowledge_label: "I understand".to_owned(),
        cancel_label: "Cancel".to_owned(),
        confirm_label: "Enable Full access".to_owned(),
    };
    let gated = SelectOption {
        confirmation: Some(confirmation.clone()),
        ..option("full", "Full access", None)
    };
    assert_eq!(gated.confirmation, Some(confirmation));

    assert_eq!(COMMAND_NS, "command");
    assert_eq!(COMMAND_LOCALES.len(), 7);
    assert_eq!(
        COMMAND_LOCALES[3],
        ("status.applying", "正在应用…", "Applying…")
    );
    assert_eq!(
        COMMAND_LOCALES[6],
        ("listbox.aria", "/{command} 匹配项", "/{command} matches")
    );
}
