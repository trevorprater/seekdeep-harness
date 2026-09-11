//! Source differential fixtures and full pinned package README corpus.

use std::process::Command;

use indexmap::IndexMap;
use seekdeep_repository_tools::package_readme_model_experience::{
    ModelExperiencePolicy, SentenceContract, SentenceKind, inspect_package_readme_model_experience,
    inspect_with_policy, render_report,
};
use tempfile::TempDir;

const SOURCE: &str = "/Users/trevor/ws/deepseek-harness";
const STRUCTURED: &str = "# Package\n\n## Model Experience\n\n### Tool result\n\n#### What the model sees\n\nThe model sees `result`.\n\n#### Token effect\n\nThe result uses one token.\n\n#### KV Cache effect\n\nEarlier messages remain stable.\n";
const SHORT: &str = "# Package\n\n## Model Experience\n\nNone, as the package only checks files.\n\n#### KV Cache effect\n\nNo request changes.\n";

fn write(root: &std::path::Path, path: &str, contents: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn oracle(root: &std::path::Path, policy: Option<&ModelExperiencePolicy>) -> (bool, String) {
    let directory = TempDir::new().unwrap();
    let mut source = std::fs::read_to_string(format!(
        "{SOURCE}/scripts/verify-package-readme-model-experience.ts"
    ))
    .unwrap()
    .replace(
        "from './markdown.ts'",
        &format!("from 'file://{SOURCE}/scripts/markdown.ts'"),
    )
    .replace(
        "const root = resolve(import.meta.dirname, '..')",
        &format!(
            "const root = {}",
            serde_json::to_string(&root.to_string_lossy()).unwrap()
        ),
    );
    if let Some(policy) = policy {
        let start = source.find("const NO_MODEL_EXPERIENCE_SECTION:").unwrap();
        let end = source.find("interface Failure").unwrap();
        source.replace_range(
            start..end,
            &format!(
                "const NO_MODEL_EXPERIENCE_SECTION = {}\nconst SENTENCE_MODEL_EXPERIENCE = {}\n\n",
                serde_json::to_string(&policy.omissions).unwrap(),
                serde_json::to_string(&policy.sentences).unwrap()
            ),
        );
    }
    let script = directory.path().join("model-experience.mts");
    std::fs::write(&script, source).unwrap();
    let output = Command::new("node").arg(&script).output().unwrap();
    let text = if output.status.success() {
        &output.stdout
    } else {
        &output.stderr
    };
    (
        output.status.success(),
        String::from_utf8(text.clone()).unwrap(),
    )
}

fn compare_fixture(readme: Option<&str>, policy: &ModelExperiencePolicy) {
    let root = TempDir::new().unwrap();
    write(
        root.path(),
        "docs/tool-catalog.md",
        "# Tools\n\n## Tool One\n",
    );
    write(root.path(), "packages/test/sample/package.json", "{}\n");
    if let Some(readme) = readme {
        write(root.path(), "packages/test/sample/README.md", readme);
    }
    let native = inspect_with_policy(root.path(), policy).unwrap();
    assert_eq!(
        (native.failures.is_empty(), render_report(&native)),
        oracle(root.path(), Some(policy)),
        "fixture {readme:?}"
    );
}

#[test]
fn structured_and_invalid_heading_field_literal_fixtures_match_source() {
    let policy = ModelExperiencePolicy {
        omissions: IndexMap::new(),
        sentences: IndexMap::new(),
    };
    compare_fixture(None, &policy);
    let fixtures = vec![
        STRUCTURED.to_owned(),
        "# Package\n".to_owned(),
        STRUCTURED.replace("## Model Experience", "### Model Experience"),
        STRUCTURED.replace("## Model Experience", "## model experience"),
        STRUCTURED.replace("## Model Experience", "## Model Experience ##"),
        STRUCTURED.replace("## Model Experience", "Model Experience\n----------------"),
        format!("{STRUCTURED}\n## Model Experience\n"),
        format!("{STRUCTURED}\n## More\n"),
        format!("{STRUCTURED}\n## Known Limitations and Deferred Work\n\n- Item\n"),
        format!("{STRUCTURED}\n## Known Limitations and Deferred Work\n\n- Item\n\n## More\n"),
        STRUCTURED.replace("### Tool result", "### !!!"),
        STRUCTURED.replace("### Tool result", "Intro paragraph\n\n### Tool result"),
        STRUCTURED.replace("### Tool result", "None, as it is generic."),
        STRUCTURED.replace("#### Token effect", "#### Wrong field"),
        STRUCTURED.replace("The result uses one token.", ""),
        STRUCTURED.replace("#### Token effect\n\n", "#### Token effect\n"),
        STRUCTURED.replace("The model sees `result`.", "The model sees text."),
        STRUCTURED.replace("The model sees `result`.", "The model sees [this](#local)."),
        STRUCTURED.replace(
            "The model sees `result`.",
            "The model sees `result`.\n\nExtra paragraph.",
        ),
        STRUCTURED.replace("### Tool result", "### System prompt"),
        STRUCTURED.replace("### Tool result", "### Tool schemas"),
        STRUCTURED
            .replace("### Tool result", "### Tool schemas")
            .replace(
                "The model sees `result`.",
                "See [catalog](../../../docs/tool-catalog.md#tool-one).",
            ),
        STRUCTURED
            .replace("### Tool result", "### Tool schemas")
            .replace(
                "The model sees `result`.",
                "See [catalog](../../../docs/tool-catalog.md#missing).",
            ),
        STRUCTURED.replace("\n\n#### Token effect", "\n#### Token effect"),
        STRUCTURED.trim_end().to_owned(),
        format!(
            "{STRUCTURED}\n{}",
            STRUCTURED
                .split_once("### Tool result")
                .unwrap()
                .1
                .replace("#### What", "### Tool result\n\n#### What")
        ),
        STRUCTURED.replace(
            "The model sees `result`.",
            "The model sees `result`.\n\n<!-- ignored -->",
        ),
    ];
    for fixture in fixtures {
        compare_fixture(Some(&fixture), &policy);
    }
}

#[test]
fn exact_nested_markdown_fences_and_duplicate_fragments_match_source() {
    let policy = ModelExperiencePolicy {
        omissions: IndexMap::new(),
        sentences: IndexMap::new(),
    };
    let valid = STRUCTURED
        .replace("### Tool result", "### System prompt")
        .replace(
            "The model sees `result`.",
            "The model sees this prompt.\n\n##### Prompt text\n\n```markdown\nExact prompt.\n```",
        );
    for fixture in [
        valid.clone(),
        valid.replace("```markdown", "```md"),
        valid
            .replace("```markdown", "~~~markdown")
            .replace("\n```", "\n~~~"),
        valid.replace("Exact prompt.\n", ""),
        valid.replace("\n```\n", "\n"),
        valid.replace("##### Prompt text", "##### !!!"),
        valid.replace(
            "The result uses one token.",
            "The result uses one token.\n\n##### Prompt text\n\n```markdown\nAgain.\n```",
        ),
        valid.replace("##### Prompt text\n\n", "##### Prompt text\n"),
        valid.replace(
            "Exact prompt.",
            "## Model Experience\n\n### Fenced headings are not prose",
        ),
    ] {
        compare_fixture(Some(&fixture), &policy);
    }
}

#[test]
fn audited_short_forms_omissions_and_policy_diagnostics_match_source() {
    let mut policy = ModelExperiencePolicy {
        omissions: IndexMap::new(),
        sentences: IndexMap::new(),
    };
    policy.sentences.insert(
        "packages/test/sample".to_owned(),
        SentenceContract {
            kind: SentenceKind::None,
            reason: "No context".to_owned(),
        },
    );
    for fixture in [
        SHORT.to_owned(),
        SHORT.replace("None, as ", "Indirectly, through "),
        SHORT.replace("None, as ", "None."),
        SHORT.replace("No request changes.", "#### Another heading"),
        SHORT.replace("\n\n####", "\n####"),
        SHORT.replace("#### KV Cache effect", "#### KV-cache effect"),
        SHORT.replace("only checks files.", "only checks files"),
        SHORT.replace(
            "No request changes.",
            "No request changes.\n\n```markdown\nhidden\n```",
        ),
        SHORT.replace(
            "No request changes.",
            "No request changes.\n<!-- hidden -->",
        ),
    ] {
        compare_fixture(Some(&fixture), &policy);
    }
    policy
        .sentences
        .get_mut("packages/test/sample")
        .unwrap()
        .kind = SentenceKind::Indirect;
    compare_fixture(
        Some(&SHORT.replace("None, as ", "Indirectly, through ")),
        &policy,
    );
    policy
        .omissions
        .insert("packages/test/sample".to_owned(), String::new());
    policy
        .omissions
        .insert("packages/test/missing".to_owned(), "Missing".to_owned());
    policy
        .sentences
        .get_mut("packages/test/sample")
        .unwrap()
        .reason
        .clear();
    compare_fixture(Some(SHORT), &policy);
    policy.sentences.clear();
    compare_fixture(Some("# Package\n"), &policy);
    compare_fixture(
        Some("# Package\n\n### Model Experience\n\n## MODEL Experience\n"),
        &policy,
    );
}

#[test]
fn pinned_source_corpus_and_real_native_cli_match_source_checker() {
    let native = inspect_package_readme_model_experience(std::path::Path::new(SOURCE)).unwrap();
    assert_eq!(
        (native.failures.is_empty(), render_report(&native)),
        oracle(std::path::Path::new(SOURCE), None)
    );
    assert!(native.checked >= 170, "{} packages", native.checked);
    assert!(native.failures.is_empty(), "{}", render_report(&native));
    let output = Command::new(env!("CARGO_BIN_EXE_verify-package-readme-model-experience"))
        .args(["--root", SOURCE])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        render_report(&native)
    );
}
