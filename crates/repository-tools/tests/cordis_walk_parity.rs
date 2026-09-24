//! The Cordis merge scan reads every module block, both identity spellings,
//! and the member forms the source scan accepts.

use seekdeep_repository_tools::cordis_walk::{context_merge_blocks, merge_blocks_in};

const MERGES: &str = "import type {} from 'x'\n\ndeclare module '@deepseek-ai/cordis' {\n  interface Context {\n    /** doc */\n    llm: LlmService\n    optional?: Maybe | undefined\n    method(): void\n    'quoted': Q\n  }\n  interface Events {\n    'llm/request'(this: Context, request: Request): void\n    plain: (value: number) => void\n    [computed]: string\n    'method/sig'(): void\n  }\n}\n\ndeclare module \"@seekdeep-ai/cordis\" {\n  export interface Context { second: Second }\n}\n\ndeclare module './context.ts' {\n  interface Context { vendor: Vendor }\n  interface Other { skipped: Skipped }\n}\n\ndeclare module 'unrelated' {\n  interface Context { ghost: Ghost }\n}\n";

#[test]
fn reads_every_cordis_module_block_with_property_keys_and_event_names() {
    let blocks = merge_blocks_in("packages/a/b/src/index.ts", MERGES);
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        blocks[0]
            .context_keys
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>(),
        ["llm=LlmService", "optional=Maybe | undefined", "'quoted'=Q"]
    );
    assert_eq!(
        blocks[0].event_names,
        ["llm/request", "plain", "[computed]", "method/sig"]
    );
    assert_eq!(
        blocks[1].context_keys.get("second").map(String::as_str),
        Some("Second")
    );
    assert_eq!(
        blocks[2].context_keys.get("vendor").map(String::as_str),
        Some("Vendor")
    );
    assert!(blocks[2].context_keys.get("skipped").is_none());
    assert!(
        blocks
            .iter()
            .all(|block| block.rel == "packages/a/b/src/index.ts")
    );
}

#[test]
fn scans_matching_files_in_path_order_and_skips_files_without_a_merge() {
    let root = tempfile::tempdir().unwrap();
    for (rel, text) in [
        ("packages/z/z/src/index.ts", MERGES),
        (
            "packages/a/a/src/deep/nested.ts",
            "declare module '@deepseek-ai/cordis' {\n  interface Events { 'a/b': () => void }\n}\n",
        ),
        ("packages/a/a/src/plain.ts", "export const x = 1\n"),
        ("packages/a/a/src/.hidden.ts", MERGES),
        ("packages/a/a/test/outside.ts", MERGES),
    ] {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let blocks = context_merge_blocks(
        root.path(),
        &["packages/*/*/src/**/*.ts", "packages/*/*/src/**/*.tsx"],
    )
    .unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.rel.as_str())
            .collect::<Vec<_>>(),
        [
            "packages/a/a/src/deep/nested.ts",
            "packages/z/z/src/index.ts",
            "packages/z/z/src/index.ts",
            "packages/z/z/src/index.ts",
        ]
    );
    assert_eq!(blocks[0].event_names, ["a/b"]);
}
