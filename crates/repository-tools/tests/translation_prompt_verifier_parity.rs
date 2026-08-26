//! Live verifier success and runnable snapshot fixture.

use seekdeep_repository_tools::translation_prompt_verifier::verify_translation_prompt;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn live_prompt_verifier_passes() {
    let output = verify_translation_prompt(&root(), false).unwrap();
    assert!(output.contains("both directions render"));
}

#[test]
fn runnable_snapshot_matches_committed_json() {
    let output = verify_translation_prompt(&root(), true).unwrap();
    serde_json::from_str::<serde_json::Value>(&output).unwrap();
    let expected =
        root().join("scripts/snapshots/translation-prompt-v4/request-response.expected.json");
    if matches!(
        std::env::var("SEEKDEEP_SNAPSHOT").as_deref(),
        Ok("record" | "refresh")
    ) {
        std::fs::write(&expected, &output).unwrap();
    }
    assert_eq!(std::fs::read_to_string(expected).unwrap(), output);
}
