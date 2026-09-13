//! Source-oracle coverage for encoded unavailable-repository references.

use seekdeep_repository_tools::public_repository_links::{
    UnavailableRepositoryReference, find_unavailable_repository_references,
};

fn unavailable_repository() -> String {
    format!(
        "{}/{}",
        ["deepseek", "ai"].join("-"),
        ["deepseek", "harness", "sdk"].join("-")
    )
}

#[test]
fn rejects_encoded_case_varied_and_nfkc_references_only() {
    let unavailable = unavailable_repository();
    let encoded = unavailable.replace('-', "%2D").replace('/', "%2F");
    let html = unavailable.replace('/', "&#x2f;");
    let escaped = unavailable.replace('/', "\\/");
    let unicode = unavailable.replace('/', r"\u002f");
    let fullwidth = unavailable
        .chars()
        .map(|character| match character {
            'a'..='z' => char::from_u32(u32::from(character) + 0xfee0).unwrap(),
            '-' => '－',
            '/' => '／',
            other => other,
        })
        .collect::<String>();
    let source = [
        "https://github.com/deepseek-ai/deepseek-harness".to_owned(),
        format!("https://github.com/{}/issues/1", unavailable.to_uppercase()),
        format!("https://github.com/{encoded}/issues/2"),
        format!("https://github.com/{html}/issues/3"),
        format!(r#""https:\/\/github.com\/{escaped}\/issues\/4""#),
        format!(r#""https:\/\/github.com\/{unicode}\/issues\/5""#),
        format!("https://github.com/{fullwidth}/issues/6"),
        "https://github.com/deepseek-ai/cordis".to_owned(),
        "https://github.com/example/deepseek-harness-sdk".to_owned(),
    ]
    .join("\n");
    assert_eq!(
        find_unavailable_repository_references("subject.md", &source),
        (2..=7)
            .map(|line| UnavailableRepositoryReference {
                file: "subject.md".into(),
                line,
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn frozen_archived_notes_are_exempt_but_active_notes_are_not() {
    let source = format!("https://github.com/{}", unavailable_repository());
    assert!(
        find_unavailable_repository_references(
            ".agents/notes/archived/process/historical-record.md",
            &source,
        )
        .is_empty()
    );
    assert_eq!(
        find_unavailable_repository_references(
            ".agents/notes/implemented/process/active-record.md",
            &source,
        ),
        [UnavailableRepositoryReference {
            file: ".agents/notes/implemented/process/active-record.md".into(),
            line: 1,
        }]
    );
}
