//! Rendering and interpolation parity specifications.

use indexmap::IndexMap;
use seekdeep_llm::ContextSnapshotSection;
use seekdeep_system_prompt::{
    AssembledContext, AssembledSection, PromptAssembly, join_context_sections,
    render_context_sections, render_context_snapshot, render_prompt,
};

fn assembly(text: &str, variables: IndexMap<String, Option<String>>) -> PromptAssembly {
    PromptAssembly {
        sections: vec![AssembledSection {
            name: "s".to_owned(),
            text: text.to_owned(),
        }],
        variables,
        ..PromptAssembly::default()
    }
}

#[test]
fn renders_prompt_and_context_while_filtering_empty_contributions() {
    let value = PromptAssembly {
        sections: vec![
            AssembledSection {
                name: "empty".to_owned(),
                text: String::new(),
            },
            AssembledSection {
                name: "persona".to_owned(),
                text: "You run on {{model}}.".to_owned(),
            },
        ],
        contexts: vec![
            AssembledContext {
                name: "empty".to_owned(),
                text: String::new(),
            },
            AssembledContext {
                name: "cwd".to_owned(),
                text: "Working in {{cwd}}".to_owned(),
            },
        ],
        variables: IndexMap::from([
            ("model".to_owned(), Some("seekdeep-v4".to_owned())),
            ("cwd".to_owned(), Some("/work".to_owned())),
        ]),
        ..PromptAssembly::default()
    };
    assert_eq!(render_prompt(&value).unwrap(), "You run on seekdeep-v4.");
    assert_eq!(
        render_context_sections(&value).unwrap(),
        [ContextSnapshotSection {
            name: "cwd".to_owned(),
            text: "Working in /work".to_owned(),
        }]
    );
    assert_eq!(
        render_context_snapshot(&value).unwrap(),
        "Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\nWorking in /work"
    );
    assert_eq!(join_context_sections(&[]), "");
}

#[test]
fn reports_unknown_undefined_and_malformed_references_with_owner_attribution() {
    let unknown = render_prompt(&assembly(
        "{{missing}}",
        IndexMap::from([("model".to_owned(), Some("m".to_owned()))]),
    ))
    .unwrap_err()
    .to_string();
    assert!(unknown.contains("unknown prompt variable \"{{missing}}\" in section \"s\""));
    assert!(unknown.contains("registered variables: model"));

    let none = render_prompt(&assembly("{{x}}", IndexMap::new()))
        .unwrap_err()
        .to_string();
    assert!(none.contains("registered variables: (none)"));

    let undefined = render_prompt(&assembly(
        "in {{cwd}}",
        IndexMap::from([("cwd".to_owned(), None)]),
    ))
    .unwrap_err()
    .to_string();
    assert!(
        undefined
            .contains("prompt variable \"{{cwd}}\" has no value for this assembly (section \"s\")")
    );

    for text in ["on {{ model }}", "{{{model}}}", "x {{a{b}} y {{model}}"] {
        let error = render_prompt(&assembly(
            text,
            IndexMap::from([("model".to_owned(), Some("m".to_owned()))]),
        ))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("malformed prompt variable reference"),
            "{error:#}"
        );
    }

    let context = PromptAssembly {
        contexts: vec![AssembledContext {
            name: "policy".to_owned(),
            text: "{{missing}}".to_owned(),
        }],
        ..PromptAssembly::default()
    };
    assert!(
        render_context_sections(&context)
            .unwrap_err()
            .to_string()
            .contains("in context \"policy\"")
    );
}

#[test]
fn preserves_literal_openers_prototype_names_and_substitution_text() {
    assert_eq!(
        render_prompt(&assembly("shell ${X:-{{fallback} stays", IndexMap::new())).unwrap(),
        "shell ${X:-{{fallback} stays"
    );
    let prototype = render_prompt(&assembly(
        "on {{constructor}}",
        IndexMap::from([("model".to_owned(), Some("m".to_owned()))]),
    ))
    .unwrap_err();
    assert!(
        prototype
            .to_string()
            .contains("unknown prompt variable \"{{constructor}}\"")
    );
    assert_eq!(
        render_prompt(&assembly(
            "{{constructor}}",
            IndexMap::from([("constructor".to_owned(), Some("own-value".to_owned()))]),
        ))
        .unwrap(),
        "own-value"
    );
    assert_eq!(
        render_prompt(&assembly(
            "v = {{model}}!",
            IndexMap::from([(
                "model".to_owned(),
                Some("literal {{sneaky}} inside".to_owned()),
            )]),
        ))
        .unwrap(),
        "v = literal {{sneaky}} inside!"
    );
}
