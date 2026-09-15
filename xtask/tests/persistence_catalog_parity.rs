//! Source-oracle negative paths for the Rust persistence-catalog generator.

use std::{collections::BTreeMap, path::Path};

use tempfile::TempDir;
use xtask::persistence_catalog::{
    EventEnvelopeTypeEntry, LogEventEntry, annotate_surface, collect_event_envelope_types,
    collect_log_events, collect_surface_event_types, render,
};

const OWNER_MANIFEST: &str = "{ \"name\": \"@deepseek-ai/dsh-session\" }\n";

fn fixture(files: &[(&str, &str)]) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    for (relative, source) in files {
        let path = root.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    root
}

fn merge(members: &str) -> String {
    format!(
        "declare module '@deepseek-ai/dsh-session/types' {{\n  interface SessionEventMap {{\n{members}\n  }}\n}}\n"
    )
}

fn error(result: anyhow::Result<impl Sized>) -> String {
    format!("{:#}", result.err().expect("expected failure"))
}

#[test]
fn collects_owning_merged_and_multiline_event_declarations() {
    let root = fixture(&[
        ("packages/core/fix/package.json", OWNER_MANIFEST),
        (
            "packages/core/fix/src/types.ts",
            "export interface SessionEventMap {\n  /** A thing was recorded. */\n  'fix/happened': { turn: number }\n}\n",
        ),
        (
            "packages/group/fix/src/types.ts",
            &merge(
                "    /** Wide payload. */\n    'fix/wide': {\n      /** Alpha values. */\n      alpha: string[]\n      range: { start: number; end: number }\n      count: number\n    }",
            ),
        ),
    ]);
    let events = collect_log_events(root.path()).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].name, "fix/happened");
    assert_eq!(events[0].scope, "fix");
    assert_eq!(events[0].doc, "A thing was recorded.");
    assert_eq!(events[0].payload, "{ turn: number }");
    assert_eq!(
        events[0].declaration,
        "/** A thing was recorded. */\n'fix/happened': { turn: number }"
    );
    assert_eq!(events[0].source, "packages/core/fix/src/types.ts:3");
    assert_eq!(
        events[1].payload,
        "{ alpha: string[]; range: { start: number; end: number }; count: number }"
    );
    assert_eq!(
        events[1].declaration,
        "/** Wide payload. */\n'fix/wide': {\n  /** Alpha values. */\n  alpha: string[]\n  range: { start: number; end: number }\n  count: number\n}"
    );
}

#[test]
fn rejects_wrong_owning_interfaces_exports_duplicates_and_inheritance() {
    let alien = fixture(&[
        (
            "packages/group/alien/package.json",
            "{ \"name\": \"@deepseek-ai/dsh-alien\" }\n",
        ),
        (
            "packages/group/alien/src/types.ts",
            "export interface SessionEventMap {\n  /** Wrong owner. */\n  'alien/event': { turn: number }\n}\n",
        ),
    ]);
    let message = error(collect_log_events(alien.path()));
    assert!(message.contains("outside @deepseek-ai/dsh-session"));
    assert!(message.contains("package @deepseek-ai/dsh-alien"));

    let unexported = fixture(&[
        ("packages/core/fix/package.json", OWNER_MANIFEST),
        (
            "packages/core/fix/src/helper.ts",
            "interface SessionEventMap {\n  /** Local. */\n  'fix/local': { turn: number }\n}\nexport const use: SessionEventMap | null = null\n",
        ),
    ]);
    assert!(error(collect_log_events(unexported.path())).contains("is not exported"));

    let duplicate = fixture(&[
        ("packages/core/fix/package.json", OWNER_MANIFEST),
        (
            "packages/core/fix/src/a.ts",
            "export interface SessionEventMap {\n  /** First. */\n  'fix/a': { turn: number }\n}\n",
        ),
        (
            "packages/core/fix/src/b.ts",
            "export interface SessionEventMap {\n  /** Second. */\n  'fix/b': { turn: number }\n}\n",
        ),
    ]);
    assert!(
        error(collect_log_events(duplicate.path()))
            .contains("already declared at packages/core/fix/src/a.ts:1")
    );

    let inherited = fixture(&[(
        "packages/group/fix/src/types.ts",
        "interface Extra { 'fix/hidden': { turn: number } }\ndeclare module '@deepseek-ai/dsh-session/types' {\n  interface SessionEventMap extends Extra {\n    /** Direct. */\n    'fix/direct': { turn: number }\n  }\n}\n",
    )]);
    assert!(error(collect_log_events(inherited.path())).contains("uses extends"));
}

#[test]
fn aggregates_member_shape_documentation_mode_and_duplicate_failures() {
    for (member, expected) in [
        (
            "    'fix/undocumented': { turn: number }",
            "no description prose",
        ),
        (
            "    /**\n     * Documented, but mistagged.\n     *   @mode emit\n     */\n    'fix/tagged': { turn: number }",
            "carries an @mode tag",
        ),
        (
            "    /** Wrong shape. */\n    'fix/method'(turn: number): void",
            "not a property signature with an explicit payload type",
        ),
        (
            "    /** No payload. */\n    'fix/bare'",
            "not a property signature with an explicit payload type",
        ),
        (
            "    /** Not literal. */\n    unquoted: { turn: number }",
            "non-literal name",
        ),
    ] {
        let source = merge(member);
        let root = fixture(&[("packages/group/fix/src/types.ts", &source)]);
        assert!(error(collect_log_events(root.path())).contains(expected));
    }

    let duplicate_a = merge("    /** First. */\n    'fix/dup': { turn: number }");
    let duplicate_b = merge("    /** Second. */\n    'fix/dup': { turn: number }");
    let duplicate = fixture(&[
        ("packages/group/fix/src/a.ts", &duplicate_a),
        ("packages/group/fix/src/b.ts", &duplicate_b),
    ]);
    assert!(
        error(collect_log_events(duplicate.path()))
            .contains("already declared at packages/group/fix/src/a.ts")
    );

    let aggregate = merge("    'fix/one': { turn: number }\n    'fix/two': { turn: number }");
    let aggregate = fixture(&[("packages/group/fix/src/types.ts", &aggregate)]);
    let message = error(collect_log_events(aggregate.path()));
    assert!(message.contains("2 JSDoc completeness violation(s)"));
    assert!(message.contains("fix/one"));
    assert!(message.contains("fix/two"));
}

fn envelope_declarations() -> &'static str {
    "/** Event keys. */\nexport type SessionEventType = keyof SessionEventMap\n/** Surface-producing event keys. */\nexport type SurfaceEventType = 'fix/message'\n/** Surface placement. */\nexport type SurfaceOp = 'append'\n/** One persisted event. */\nexport type SessionEvent<T extends SessionEventType = SessionEventType> = { type: T }\n"
}

#[test]
fn validates_event_envelope_declarations_as_one_exported_documented_owner() {
    let root = fixture(&[
        ("packages/core/fix/package.json", OWNER_MANIFEST),
        ("packages/core/fix/src/types.ts", envelope_declarations()),
    ]);
    let entries = collect_event_envelope_types(root.path()).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        [
            "SessionEventType",
            "SurfaceEventType",
            "SurfaceOp",
            "SessionEvent"
        ]
    );
    assert_eq!(entries[3].source, "packages/core/fix/src/types.ts:8");
    assert_eq!(
        entries[3].declaration,
        "/** One persisted event. */\nexport type SessionEvent<T extends SessionEventType = SessionEventType> = { type: T }"
    );

    let missing_source = envelope_declarations().replace(
        "/** Surface placement. */\nexport type SurfaceOp = 'append'\n",
        "",
    );
    let missing = fixture(&[
        ("packages/core/fix/package.json", OWNER_MANIFEST),
        ("packages/core/fix/src/types.ts", &missing_source),
    ]);
    assert!(
        error(collect_event_envelope_types(missing.path()))
            .contains("missing event-envelope declaration(s): SurfaceOp")
    );

    let violations_source = envelope_declarations()
        .replace(
            "/** Event keys. */\nexport type SessionEventType",
            "/** Event keys.\n * @mode emit\n */\ntype SessionEventType",
        )
        .replace("/** Surface placement. */\n", "")
        + "/** Duplicate event. */\nexport type SessionEvent = { type: never }\n";
    let violations = fixture(&[
        ("packages/core/fix/package.json", OWNER_MANIFEST),
        ("packages/core/fix/src/types.ts", &violations_source),
    ]);
    let message = error(collect_event_envelope_types(violations.path()));
    assert!(message.contains("4 JSDoc completeness violation(s)"));
    assert!(message.contains("not exported"));
    assert!(message.contains("@mode tag"));
    assert!(message.contains("SurfaceOp") && message.contains("no description prose"));
    assert!(message.contains("SessionEvent") && message.contains("already declared"));
}

#[test]
fn validates_the_single_closed_surface_union() {
    let root = fixture(&[(
        "packages/core/fix/src/types.ts",
        "export type SurfaceEventType = 'fix/a' | 'fix/b'\n",
    )]);
    assert_eq!(
        collect_surface_event_types(root.path()).unwrap(),
        ["fix/a", "fix/b"]
    );

    let missing = fixture(&[(
        "packages/core/fix/src/types.ts",
        "export const unrelated = 1\n",
    )]);
    assert!(
        error(collect_surface_event_types(missing.path()))
            .contains("no SurfaceEventType union found")
    );

    let duplicate = fixture(&[
        (
            "packages/core/fix/src/a.ts",
            "export type SurfaceEventType = 'fix/a'\n",
        ),
        (
            "packages/core/fix/src/b.ts",
            "export type SurfaceEventType = 'fix/b'\n",
        ),
    ]);
    assert!(
        error(collect_surface_event_types(duplicate.path())).contains("declared more than once")
    );

    let invalid = fixture(&[(
        "packages/core/fix/src/types.ts",
        "export type SurfaceEventType = 'fix/a' | number\n",
    )]);
    assert!(
        error(collect_surface_event_types(invalid.path())).contains("non-string-literal member")
    );
}

fn event(name: &str) -> LogEventEntry {
    LogEventEntry {
        name: name.to_owned(),
        scope: name.split('/').next().unwrap_or(name).to_owned(),
        payload: "{ turn: number }".to_owned(),
        doc: format!("Records {name}."),
        declaration: format!("/** Records {name}. */\n'{name}': {{ turn: number }}"),
        source: "packages/core/fix/src/types.ts:3".to_owned(),
    }
}

#[test]
fn annotates_surface_and_renders_the_generated_contract() {
    let annotated = annotate_surface(
        vec![event("fix/message"), event("fix/marker")],
        &["fix/message".to_owned()],
    )
    .unwrap();
    assert_eq!(
        annotated
            .iter()
            .map(|entry| (entry.event.name.as_str(), entry.surface))
            .collect::<Vec<_>>(),
        [("fix/message", true), ("fix/marker", false)]
    );
    assert!(
        error(annotate_surface(
            vec![event("fix/marker")],
            &["fix/ghost".to_owned()]
        ))
        .contains("'fix/ghost' name no declared log event")
    );

    let envelope = [
        "SessionEventType",
        "SurfaceEventType",
        "SurfaceOp",
        "SessionEvent",
    ]
    .map(|name| EventEnvelopeTypeEntry {
        name: name.to_owned(),
        declaration: format!("/** {name}. */\nexport type {name} = never"),
        source: "packages/core/fix/src/types.ts:1".to_owned(),
    });
    let output = render(&annotated, &envelope);
    assert!(output.contains("Generated by cargo xtask persistence-catalog"));
    assert!(output.contains("# Session Persistence Event Catalog"));
    assert!(output.contains(
        "```ts persistence-catalog\n/** SessionEventType. */\nexport type SessionEventType = never"
    ));
    assert!(output.contains("#### `fix/message` — surface"));
    assert!(output.contains("#### `fix/marker` — log-only"));
    assert!(output.contains(
        "```ts persistence-catalog\n/** Records fix/marker. */\n'fix/marker': { turn: number }\n```"
    ));
}

#[test]
fn fixture_paths_use_repository_relative_forward_slashes() {
    let mut files = BTreeMap::new();
    files.insert(
        "packages/group/fix/src/types.ts",
        merge("    /** Event. */\n    'fix/event': { value: string }"),
    );
    let pairs = files
        .iter()
        .map(|(path, source)| (*path, source.as_str()))
        .collect::<Vec<_>>();
    let root = fixture(&pairs);
    assert_eq!(
        collect_log_events(Path::new(root.path())).unwrap()[0].source,
        "packages/group/fix/src/types.ts:4"
    );
}
