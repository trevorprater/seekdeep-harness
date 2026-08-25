//! Config-catalog classification, closure, schema, and rendering parity.

use std::path::Path;

use xtask::config_catalog::{CatalogKind, collect_config_catalog, render};

const DOCUMENTED_CONFIG: &str =
    "/** Fixture config. */\nexport interface Config {\n  /** A knob. */\n  knob?: string\n}\n";

fn write_package(root: &Path, directory: &str, name: Option<&str>, files: &[(&str, &str)]) {
    let package = root.join("packages").join(directory);
    std::fs::create_dir_all(package.join("src")).unwrap();
    let manifest = name.map_or_else(|| "{}".to_owned(), |name| format!(r#"{{"name":"{name}"}}"#));
    std::fs::write(package.join("package.json"), manifest).unwrap();
    for (relative, source) in files {
        let path = package.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, source).unwrap();
    }
}

fn one(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write_package(root.path(), "group/one", Some("@fix/one"), files);
    root
}

fn error(root: &Path) -> String {
    collect_config_catalog(root).unwrap_err().to_string()
}

#[test]
fn classifies_apply_config_and_extracts_inject_and_paste() {
    let source = format!(
        "import type {{ Context }} from '@deepseek-ai/cordis'\nexport const inject = ['tools']\n{DOCUMENTED_CONFIG}/** Load. */\nexport function apply(ctx: Context, config: Config): void {{}}\n"
    );
    let root = one(&[("src/index.ts", &source)]);
    let entries = collect_config_catalog(root.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].package, "@fix/one");
    assert_eq!(entries[0].kind, CatalogKind::Config);
    assert_eq!(entries[0].config_type_name.as_deref(), Some("Config"));
    assert_eq!(entries[0].inject, ["tools"]);
    assert!(
        entries[0].pastes.as_ref().unwrap()[0]
            .text
            .contains("/** A knob. */")
    );
}

#[test]
fn classifies_default_service_class_and_static_fields() {
    let source = format!(
        "import type {{ Context }} from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\n{DOCUMENTED_CONFIG}/** Fixture service. */\nexport default class Fix {{\n  static inject = ['llm']\n  static Config = z.object({{ knob: z.string() }}) as unknown as z<Config>\n  constructor(ctx: Context, config: Config) {{}}\n}}\n"
    );
    let root = one(&[("src/index.ts", &source)]);
    let entry = &collect_config_catalog(root.path()).unwrap()[0];
    assert_eq!(entry.kind, CatalogKind::Config);
    assert_eq!(entry.class_name.as_deref(), Some("Fix"));
    assert_eq!(entry.inject, ["llm"]);
    assert_eq!(entry.schema_keys.as_ref().unwrap(), &["knob".to_owned()]);
}

#[test]
fn classifies_seam_no_config_and_library() {
    for (source, kind, class_name) in [
        (
            "export default abstract class FixSeam { abstract run(): void }\n",
            CatalogKind::Seam,
            Some("FixSeam"),
        ),
        (
            "import type { Context } from 'cordis'\n/** Load. */\nexport function apply(ctx: Context): void {}\n",
            CatalogKind::NoConfig,
            None,
        ),
        ("export const helper = 1\n", CatalogKind::Library, None),
    ] {
        let root = one(&[("src/index.ts", source)]);
        let entry = &collect_config_catalog(root.path()).unwrap()[0];
        assert_eq!(entry.kind, kind);
        assert_eq!(entry.class_name.as_deref(), class_name);
    }
}

#[test]
fn missing_entry_and_package_name_fail_loud() {
    let root = tempfile::tempdir().unwrap();
    write_package(root.path(), "group/one", Some("@fix/one"), &[]);
    assert!(
        error(root.path())
            .contains("entry packages/group/one/src/index.ts is missing or unreadable")
    );

    let root = tempfile::tempdir().unwrap();
    write_package(root.path(), "group/one", None, &[("src/index.ts", "")]);
    assert!(error(root.path()).contains("has no \"name\""));
}

#[test]
fn undocumented_top_level_and_nested_fields_fail() {
    let root = one(&[(
        "src/index.ts",
        "import type { Context } from '@deepseek-ai/cordis'\nexport interface Config {\n  knob?: string\n}\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
    )]);
    assert!(error(root.path()).contains("config field 'Config.knob'"));

    let root = one(&[(
        "src/index.ts",
        "import type { Context } from '@deepseek-ai/cordis'\n/** Fixture config. */\nexport interface Config {\n  /** Entries. */\n  entries: {\n    id: string\n  }[]\n}\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
    )]);
    assert!(error(root.path()).contains("config field 'Config.entries.id'"));
}

#[test]
fn pastes_local_closure_and_records_external_references() {
    let root = one(&[
        (
            "src/index.ts",
            "import type { Context } from '@deepseek-ai/cordis'\nimport type { Mode } from './types.ts'\nimport type { Remote } from '@fix/dep'\n/** Fixture config. */\nexport interface Config {\n  /** The mode. */\n  mode?: Mode\n  /** The remote. */\n  remote?: Remote\n}\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
        ),
        (
            "src/types.ts",
            "/** Fixture mode. */\nexport type Mode = 'a' | 'b'\n",
        ),
    ]);
    let entry = &collect_config_catalog(root.path()).unwrap()[0];
    assert_eq!(
        entry
            .pastes
            .as_ref()
            .unwrap()
            .iter()
            .map(|paste| paste.source.as_str())
            .collect::<Vec<_>>(),
        [
            "packages/group/one/src/index.ts:5",
            "packages/group/one/src/types.ts:2"
        ]
    );
    let reference = &entry.refs.as_ref().unwrap()[0];
    assert_eq!(reference.alias, "Remote");
    assert_eq!(reference.imported, "Remote");
    assert_eq!(reference.specifier, "@fix/dep");
}

#[test]
fn enum_is_pasted_after_the_config() {
    let root = one(&[(
        "src/index.ts",
        "import type { Context } from '@deepseek-ai/cordis'\n/** Fixture mode. */\nexport enum Mode {\n  A = 'a',\n  B = 'b',\n}\n/** Fixture config. */\nexport interface Config {\n  /** The mode. */\n  mode?: Mode\n}\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
    )]);
    let pastes = collect_config_catalog(root.path()).unwrap()[0]
        .pastes
        .clone()
        .unwrap();
    assert!(pastes[0].text.starts_with("/** Fixture config. */"));
    assert!(pastes[1].text.starts_with("/** Fixture mode. */"));
    assert!(pastes[1].text.contains("export enum Mode"));
}

#[test]
fn unknown_and_imported_config_types_fail() {
    let root = one(&[(
        "src/index.ts",
        "import type { Context } from '@deepseek-ai/cordis'\n/** Fixture config. */\nexport interface Config {\n  /** The ghost. */\n  ghost?: Ghost\n}\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
    )]);
    assert!(error(root.path()).contains("references 'Ghost'"));

    let root = one(&[(
        "src/index.ts",
        "import type { Context } from '@deepseek-ai/cordis'\nimport type { Config } from '@fix/dep'\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
    )]);
    assert!(error(root.path()).contains("config type 'Config' is imported from '@fix/dep'"));
}

#[test]
fn duplicate_names_across_the_closure_fail() {
    let root = one(&[
        (
            "src/index.ts",
            "import type { Context } from '@deepseek-ai/cordis'\nimport type { A } from './a.ts'\nimport type { B } from './b.ts'\n/** Fixture config. */\nexport interface Config {\n  /** A. */\n  a?: A\n  /** B. */\n  b?: B\n}\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
        ),
        (
            "src/a.ts",
            "/** First Option. */\nexport interface Option {\n  /** X. */\n  x?: string\n}\n/** A. */\nexport interface A {\n  /** O. */\n  o?: Option\n}\n",
        ),
        (
            "src/b.ts",
            "/** Second Option. */\nexport interface Option {\n  /** Y. */\n  y?: string\n}\n/** B. */\nexport interface B {\n  /** O. */\n  o?: Option\n}\n",
        ),
    ]);
    assert!(
        error(root.path()).contains("type name 'Option' resolves to two different declarations")
    );
}

#[test]
fn chained_schema_accepts_declared_keys_and_rejects_missing_keys() {
    let valid = format!(
        "import type {{ Context }} from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\n{DOCUMENTED_CONFIG}export const Config: z<Config> = z.object({{ knob: z.string() }}).default({{}})\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {{}}\n"
    );
    let root = one(&[("src/index.ts", &valid)]);
    assert_eq!(
        collect_config_catalog(root.path()).unwrap()[0]
            .schema_keys
            .as_ref()
            .unwrap(),
        &["knob".to_owned()]
    );

    let invalid = format!(
        "import type {{ Context }} from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\n{DOCUMENTED_CONFIG}export const Config: z<Config> = z.object({{ knob: z.string(), hidden: z.number() }})\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {{}}\n"
    );
    let root = one(&[("src/index.ts", &invalid)]);
    assert!(error(root.path()).contains("schema validates key 'hidden'"));
}

#[test]
fn nested_schema_miss_is_reported_with_array_path() {
    let root = one(&[(
        "src/index.ts",
        "import type { Context } from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\n/** Fixture config. */\nexport interface Config {\n  /** Entries. */\n  entries: {\n    /** Id. */\n    id: string\n  }[]\n}\nexport const Config: z<Config> = z.object({ entries: z.array(z.object({ id: z.string(), ghost: z.string() })) })\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
    )]);
    assert!(error(root.path()).contains("schema validates key 'entries[].ghost'"));
}

#[test]
fn workspace_intersection_and_reexport_resolve_nested_keys() {
    let root = tempfile::tempdir().unwrap();
    write_package(
        root.path(),
        "group/dep",
        Some("@fix/dep"),
        &[
            ("src/index.ts", "export * from './types.ts'\n"),
            (
                "src/types.ts",
                "/** Shared options. */\nexport interface Opts {\n  /** Model. */\n  model?: string\n}\n",
            ),
        ],
    );
    write_package(
        root.path(),
        "group/one",
        Some("@fix/one"),
        &[(
            "src/index.ts",
            "import type { Context } from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\nimport type { Opts } from '@fix/dep'\n/** Fixture config. */\nexport interface Config {\n  /** Entries. */\n  entries: (Opts & {\n    /** Id. */\n    id: string\n  })[]\n}\nexport const Config: z<Config> = z.object({ entries: z.array(z.object({ id: z.string(), model: z.string() })) })\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
        )],
    );
    collect_config_catalog(root.path()).unwrap();
}

#[test]
fn partial_wrapper_resolves_and_external_nested_type_stays_unknown() {
    let root = one(&[(
        "src/index.ts",
        "import type { Context } from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\n/** Caps. */\nexport interface Caps {\n  /** X. */\n  x?: boolean\n}\n/** Fixture config. */\nexport interface Config {\n  /** Capabilities. */\n  capabilities?: Partial<Caps>\n}\nexport const Config: z<Config> = z.object({ capabilities: z.object({ x: z.boolean() }) })\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
    )]);
    collect_config_catalog(root.path()).unwrap();

    let root = one(&[(
        "src/index.ts",
        "import type { Context } from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\nimport type { External } from 'some-external-pkg'\n/** Fixture config. */\nexport interface Config {\n  /** Options. */\n  options?: External\n}\nexport const Config: z<Config> = z.object({ options: z.object({ whatever: z.string() }) })\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
    )]);
    collect_config_catalog(root.path()).unwrap();
}

fn write_leaf(root: &Path, nested: bool) {
    let config = if nested {
        "/** Leaf config. */\nexport interface Config {\n  /** Agents. */\n  agents: {\n    /** Id. */\n    id: string\n  }[]\n}"
    } else {
        "/** Leaf config. */\nexport interface Config {\n  /** Leaf knob. */\n  leaf?: string\n}"
    };
    let schema = if nested {
        "z.object({ agents: z.array(z.object({ id: z.string() })) })"
    } else {
        "z.object({ leaf: z.string() })"
    };
    let source = format!(
        "import type {{ Context }} from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\n{config}\n/** Leaf service. */\nexport default class Leaf {{\n  static Config = {schema} as unknown as z<Config>\n  constructor(ctx: Context, config: Config) {{}}\n}}\n"
    );
    write_package(
        root,
        "group/leaf",
        Some("@fix/leaf"),
        &[("src/index.ts", &source)],
    );
}

#[test]
fn composed_schema_is_folded_and_missing_forwarder_fails() {
    let root = tempfile::tempdir().unwrap();
    write_leaf(root.path(), false);
    write_package(
        root.path(),
        "group/bundle",
        Some("@fix/bundle"),
        &[(
            "src/index.ts",
            "import type { Context } from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\nimport Leaf from '@fix/leaf'\n/** Bundle config. */\nexport interface Config {\n  /** Forwarded leaf knob. */\n  leaf?: string\n}\nexport const Config = z.intersect([Leaf.Config]) as unknown as z<Config>\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
        )],
    );
    let entries = collect_config_catalog(root.path()).unwrap();
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.package == "@fix/bundle")
            .unwrap()
            .schema_composes,
        ["@fix/leaf"]
    );

    let root = tempfile::tempdir().unwrap();
    write_leaf(root.path(), false);
    write_package(
        root.path(),
        "group/bundle",
        Some("@fix/bundle"),
        &[(
            "src/index.ts",
            "import type { Context } from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\nimport Leaf from '@fix/leaf'\n/** Bundle config that forgot the field. */\nexport interface Config {\n  /** Other. */\n  other?: string\n}\nexport const Config = z.intersect([Leaf.Config]) as unknown as z<Config>\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
        )],
    );
    assert!(error(root.path()).contains("schema validates key 'leaf'"));
}

#[test]
fn indexed_access_forwarder_resolves_composed_nested_schema() {
    let root = tempfile::tempdir().unwrap();
    write_leaf(root.path(), true);
    write_package(
        root.path(),
        "group/bundle",
        Some("@fix/bundle"),
        &[(
            "src/index.ts",
            "import type { Context } from '@deepseek-ai/cordis'\nimport z from '@deepseek-ai/schemastery'\nimport Leaf, { type Config as LeafConfig } from '@fix/leaf'\n/** Bundle config. */\nexport interface Config {\n  /** Forwarded agents. */\n  agents?: LeafConfig['agents']\n}\nexport const Config = z.intersect([Leaf.Config]) as unknown as z<Config>\n/** Load. */\nexport function apply(ctx: Context, config: Config): void {}\n",
        )],
    );
    collect_config_catalog(root.path()).unwrap();
}

#[test]
fn render_contains_config_fence_and_terse_lists() {
    let root = tempfile::tempdir().unwrap();
    let config_source = format!(
        "import type {{ Context }} from '@deepseek-ai/cordis'\n{DOCUMENTED_CONFIG}/** Load. */\nexport function apply(ctx: Context, config: Config): void {{}}\n"
    );
    write_package(
        root.path(),
        "group/one",
        Some("@fix/one"),
        &[("src/index.ts", &config_source)],
    );
    write_package(
        root.path(),
        "group/lib",
        Some("@fix/lib"),
        &[("src/index.ts", "export const helper = 1\n")],
    );
    write_package(
        root.path(),
        "group/seam",
        Some("@fix/seam"),
        &[(
            "src/index.ts",
            "export default abstract class Seam { abstract run(): void }\n",
        )],
    );
    let page = render(&collect_config_catalog(root.path()).unwrap());
    assert!(page.contains("## `@fix/one`"));
    assert!(page.contains("```ts config-catalog"));
    assert!(page.contains("/** A knob. */"));
    assert!(page.contains(
        "- `@fix/lib` ([`packages/group/lib/src/index.ts`](../packages/group/lib/src/index.ts))"
    ));
    assert!(page.contains("- `@fix/seam` — abstract `Seam`"));
}
