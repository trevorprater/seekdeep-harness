//! The lexical slot scan: contract declarations, registration call sites,
//! standard-kit members, and the one-level type index.

use std::path::Path;

use seekdeep_repository_tools::slot_walk::{
    ScannedFile, declared_types, index_exported_types, referenced_type_names, scan_slot_files,
    slot_declarations, slot_registrations, standard_kit_members,
};

const CONTRACT: &str = "import type { X } from 'y'\n\n/** Header comment. */\nexport interface Owner {\n  /** Width. */\n  width: number\n}\n\nexport type Alias = Owner | null\n\ndeclare module '@deepseek-ai/dsh-client-ui-slots' {\n  interface SlotMap {\n    /**\n     * A seat for demos.\n     *\n     * Second paragraph.\n     */\n    'demo.seat': {\n      kind: 'single'\n      scope: 'root'\n      owner: Owner\n      inject: DemoInject\n    }\n    // not a doc comment\n    'demo.keyed': { kind: 'keyed'; scope: 'session'; keyProps: { a: 1;\n        b: 2 }; hookContext: Ctx }\n    computed: { kind: Kind, scope: Scope }\n    plain: string\n  }\n  interface GlobalStandardProps {\n    /** Sessions hook. */\n    useSessions: Hook<\n      Sessions>\n    maybe?: number\n  }\n  interface SessionStandardProps { session: Session }\n}\n";

const REGISTRATIONS: &str = "export function apply(ctx) {\n  ctx.slots.register({ name: 'demo.seat', children: { 'demo.keyed': {}, inner: {} } }, DemoSeat)\n  slots.register({ name: 'demo.keyed', key: 'bash', id: 'ignored' }, () => React.createElement('div', null, 'a very long component expression that keeps going on and on'))\n  ctx.tools.register({ name: 'not-a-slot' }, Tool)\n  ctx.slots.register({ name: computed }, Skipped)\n  ctx.slots.register({ name: 'demo.seat', children: [] })\n  ctx.slots.register({ ['name']: 'demo.seat' }, Shorthand)\n}\n";

fn file(rel: &str, text: &str) -> ScannedFile {
    ScannedFile {
        rel: rel.to_owned(),
        package: "@deepseek-ai/dsh-client-demo".to_owned(),
        text: text.to_owned(),
    }
}

#[test]
fn reads_slot_declarations_with_literals_member_texts_and_dedented_docs() {
    let declarations = slot_declarations(&file("packages/client/demo/src/slots.ts", CONTRACT));
    assert_eq!(declarations.len(), 4);
    let seat = &declarations[0];
    assert_eq!(seat.key, "demo.seat");
    assert_eq!(
        (seat.kind.as_str(), seat.scope.as_str()),
        ("single", "root")
    );
    assert_eq!(seat.owner_type.as_deref(), Some("Owner"));
    assert_eq!(seat.inject_type.as_deref(), Some("DemoInject"));
    assert_eq!(seat.key_props, None);
    // The shared indentation of the continuation lines includes the space
    // before each `*`, exactly as the source's `dedent` strips it.
    assert_eq!(
        seat.js_doc,
        "/**\n* A seat for demos.\n*\n* Second paragraph.\n*/"
    );
    assert_eq!(seat.source, "packages/client/demo/src/slots.ts:16");
    assert_eq!(seat.package, "@deepseek-ai/dsh-client-demo");
    let keyed = &declarations[1];
    assert_eq!(keyed.key_props.as_deref(), Some("{ a: 1; b: 2 }"));
    assert_eq!(keyed.hook_context.as_deref(), Some("Ctx"));
    assert_eq!(keyed.js_doc, "");
    assert_eq!(declarations[2].kind, "");
    assert_eq!(declarations[3].key, "plain");
    assert_eq!(declarations[3].owner_type, None);
}

#[test]
fn reads_registrations_from_slots_receivers_only() {
    let registrations =
        slot_registrations(&file("packages/client/demo/src/index.ts", REGISTRATIONS));
    assert_eq!(registrations.len(), 3);
    let first = &registrations[0];
    assert_eq!(first.key, "demo.seat");
    assert_eq!(first.component, "DemoSeat");
    assert_eq!(first.children, ["demo.keyed", "inner"]);
    assert_eq!(first.source, "packages/client/demo/src/index.ts:2");
    let second = &registrations[1];
    assert_eq!(second.entry_key.as_deref(), Some("bash"));
    assert_eq!(second.id.as_deref(), Some("ignored"));
    assert_eq!(second.component.encode_utf16().count(), 58);
    assert!(second.component.ends_with('…'));
    assert_eq!(registrations[2].component, "(none)");
    assert!(registrations[2].children.is_empty());
}

#[test]
fn reads_standard_kit_members_and_indexes_exported_types_once() {
    let files = [file("packages/client/demo/src/slots.ts", CONTRACT)];
    assert_eq!(
        standard_kit_members(&files, "GlobalStandardProps"),
        ["useSessions: Hook< Sessions>", "maybe?: number"]
    );
    assert_eq!(
        standard_kit_members(&files, "SessionStandardProps"),
        ["session: Session"]
    );
    let root = tempfile::tempdir().unwrap();
    for (rel, text) in [
        ("packages/client/demo/src/slots.ts", CONTRACT),
        ("packages/client/demo/src/index.ts", REGISTRATIONS),
        (
            "packages/client/other/src/dup.ts",
            "export interface Alias {}\nexport interface Unique { owner: Owner }\n",
        ),
    ] {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    std::fs::write(
        root.path().join("packages/client/demo/package.json"),
        r#"{"name":"@deepseek-ai/dsh-client-demo"}"#,
    )
    .unwrap();
    let patterns = ["packages/*/*/src/**/*.ts", "packages/*/*/src/**/*.tsx"];
    let index = index_exported_types(root.path(), &patterns).unwrap();
    assert_eq!(index.keys().collect::<Vec<_>>(), ["Owner", "Unique"]);
    assert_eq!(
        index["Owner"].text,
        "/** Header comment. */\nexport interface Owner {\n  /** Width. */\n  width: number\n}"
    );
    assert_eq!(index["Owner"].source, "packages/client/demo/src/slots.ts:4");
    assert_eq!(
        referenced_type_names(&["uses Owner and Unique".to_owned()], &index),
        ["Owner", "Unique"]
    );
    assert!(referenced_type_names(&["OwnerProps".to_owned()], &index).is_empty());
    assert_eq!(
        declared_types(
            &["Unique".to_owned(), "Owner".to_owned(), "Ghost".to_owned()],
            &index
        )
        .iter()
        .map(|declaration| declaration.name.as_str())
        .collect::<Vec<_>>(),
        ["Owner", "Unique"]
    );
    let scanned = scan_slot_files(root.path(), &patterns).unwrap();
    assert_eq!(
        scanned
            .iter()
            .map(|file| (file.rel.as_str(), file.package.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                "packages/client/demo/src/index.ts",
                "@deepseek-ai/dsh-client-demo"
            ),
            (
                "packages/client/demo/src/slots.ts",
                "@deepseek-ai/dsh-client-demo"
            ),
        ]
    );
    let _ = Path::new("");
}
