//! Real compiler and pinned-source checks for repository TypeScript commands.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use seekdeep_repository_tools::{
    doc_typecheck::{
        CompileMode, check_documentation_with_compiler, extract_blocks, markdown_files,
        remap_block_paths,
    },
    ts_project::{RepositoryCompiler, TypeScriptProject},
    type_equiv::{
        extract_equiv_blocks, normalize_jsdoc, normalize_structure, strip_export,
        verify_type_equiv_with_compiler,
    },
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn source_root() -> PathBuf {
    PathBuf::from(
        include_str!("../../../SOURCE_SNAPSHOT")
            .lines()
            .find_map(|line| line.strip_prefix("repository="))
            .unwrap(),
    )
}

fn compiler_library() -> PathBuf {
    source_root().join("node_modules/typescript/lib/typescript.js")
}

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn fixture() -> TempDir {
    let temporary = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let root = temporary.path();
    write(root, "package.json", r#"{"private":true,"type":"module"}"#);
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        source_root().join("node_modules"),
        root.join("node_modules"),
    )
    .unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(
        source_root().join("node_modules"),
        root.join("node_modules"),
    )
    .unwrap();
    for script in [
        "doc-typecheck.ts",
        "doc-typecheck-paths.ts",
        "verify-type-equiv.ts",
        "markdown.ts",
        "paired-markdown-derivatives.ts",
        "repo-files.ts",
    ] {
        write(
            root,
            &format!("scripts/{script}"),
            &std::fs::read_to_string(source_root().join("scripts").join(script)).unwrap(),
        );
    }
    let original =
        std::fs::read_to_string(source_root().join("scripts/verify-type-equiv.ts")).unwrap();
    let prefix = original.split("const manifestRaw =").next().unwrap();
    write(
        root,
        "scripts/equiv-oracle.ts",
        &format!(
            "{prefix}\nexport {{ normalizeStructure, normalizeJSDoc, stripExport, blockSymbol, sourceDeclaration, sourcePublicApi, extractEquivBlocks }};\n"
        ),
    );
    write(
        root,
        "tsconfig.base.json",
        r#"{
  // Keep the wildcard comment delimiter inside the path untouched by JSONC parsing.
  "compilerOptions": {
    "target": "es2024", "module": "esnext", "moduleResolution": "bundler",
    "strict": true, "skipLibCheck": true, "declaration": true, "composite": true,
    "incremental": true, "noUnusedLocals": true, "noUnusedParameters": true,
    "moduleDetection": "force", "types": [],
    "paths": { "@fixture/runtime": ["./packages/runtime/src/index.ts"], "@fixture/*": ["./packages/*/src/index.ts"] }
  }
}"#,
    );
    write(
        root,
        "tsconfig.host.json",
        r#"{
  "extends": "./tsconfig.base.json",
  "compilerOptions": { "outDir": "./lib/types", "tsBuildInfoFile": "./lib/host.tsbuildinfo" },
  "include": ["empty.ts"],
  "references": [{"path":"./packages/runtime"}]
}"#,
    );
    write(root, "empty.ts", "export {};\n");
    write(
        root,
        "packages/runtime/tsconfig.json",
        r#"{
  "extends": "../../tsconfig.base.json",
  "compilerOptions": { "rootDir":"src", "outDir":"lib/types", "tsBuildInfoFile":"lib/runtime.tsbuildinfo" },
  "include": ["src/**/*.ts"]
}"#,
    );
    write(
        root,
        "packages/runtime/src/index.ts",
        "export function greet(name: string): string { return 'hello ' + name; }\nexport interface Model { value: string; }\n",
    );
    write(
        root,
        "packages/runtime/lib/types/index.d.ts",
        "export declare function greet(name: string): string;\nexport interface Model { value: string; }\n",
    );
    temporary
}

fn run_node(root: &Path, script: &str, input: &Value) -> Value {
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(input).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)))
}

fn oracle_command(root: &Path, script: &str, built: bool) -> (bool, String, String) {
    let output = Command::new("node")
        .arg(format!("scripts/{script}.ts"))
        .current_dir(root)
        .env(
            "DSH_DOC_TYPECHECK_USE_BUILD_OUTPUT",
            if built { "1" } else { "0" },
        )
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn real_built_declaration_compilation_and_diagnostics_match_source() {
    let temporary = fixture();
    let root = temporary.path();
    let code = "# Example\n\n```ts\nimport { greet } from '@fixture/runtime';\nconst value: string = greet('world');\n```\n\n```ts ignore-check\nillustration()\n```\n\n```ts type-equiv\ninterface Model { value: string; }\n```\n";
    write(root, "docs/example.md", code);
    write(root, "docs/example.zh.md", code);
    write(
        root,
        ".agents/notes/archived/proposed/skip.md",
        "```ts\nconst invalid: never = 1;\n```\n",
    );
    write(
        root,
        "packages/group/item/readme.md",
        "```ts\nconst second: number = 4;\n```\n",
    );
    write(
        root,
        "packages/group/item/deeper/unscanned.md",
        "```ts\nconst invalid: never = 1;\n```\n",
    );
    let before = declaration_files(root);
    let native =
        check_documentation_with_compiler(root, CompileMode::BuiltTypes, Some(&compiler_library()))
            .unwrap();
    assert!(native.passed, "{}", native.stderr);
    assert_eq!(native.checked, 2);
    assert_eq!(native.derivatives, 3);
    assert_eq!(
        declaration_files(root),
        before,
        "built mode must not emit declarations or caches"
    );
    let source = oracle_command(root, "doc-typecheck", true);
    assert_eq!((native.passed, native.stdout, native.stderr), source);
    write(
        root,
        "docs/example.md",
        &code.replace("greet('world')", "greet(7)"),
    );
    write(
        root,
        "docs/example.zh.md",
        &code.replace("greet('world')", "greet(7)"),
    );
    let native =
        check_documentation_with_compiler(root, CompileMode::BuiltTypes, Some(&compiler_library()))
            .unwrap();
    assert!(!native.passed);
    assert!(
        native
            .stderr
            .contains("docs/example.md (block at line 3, +2:29): error TS2345"),
        "{}",
        native.stderr
    );
    assert_eq!(
        (native.passed, native.stdout, native.stderr),
        oracle_command(root, "doc-typecheck", true)
    );
    assert!(!root.join(".doc-typecheck").exists());
}

#[test]
fn standalone_compiles_references_remaps_errors_and_cleans_temp_projects() {
    let temporary = fixture();
    let root = temporary.path();
    write(
        root,
        "README.md",
        "```ts\nimport { greet } from '@fixture/runtime';\nconst value: string = greet('world');\n```\n",
    );
    std::fs::remove_file(root.join("packages/runtime/lib/types/index.d.ts")).unwrap();
    let native =
        check_documentation_with_compiler(root, CompileMode::Standalone, Some(&compiler_library()))
            .unwrap();
    assert!(native.passed, "{}", native.stderr);
    assert!(root.join("packages/runtime/lib/types/index.d.ts").is_file());
    assert_eq!(
        (native.passed, native.stdout, native.stderr),
        oracle_command(root, "doc-typecheck", false)
    );
    write(
        root,
        "README.md",
        "```ts\nimport { greet } from '@fixture/runtime';\ngreet(2);\n```\n",
    );
    let native =
        check_documentation_with_compiler(root, CompileMode::Standalone, Some(&compiler_library()))
            .unwrap();
    assert!(!native.passed);
    assert_eq!(
        (native.passed, native.stdout, native.stderr),
        oracle_command(root, "doc-typecheck", false)
    );
    assert!(!std::fs::read_dir(root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".doc-typecheck-")
    }));
}

#[test]
fn extraction_scope_opt_out_threshold_and_empty_command_match_source() {
    let temporary = fixture();
    let root = temporary.path();
    for (name, checked, ignored) in [
        ("empty", 0, 5),
        ("three", 1, 2),
        ("half", 2, 2),
        ("excess", 1, 3),
    ] {
        let text = format!(
            "{}{}{}",
            "```ts\nexport {};\n```\n".repeat(checked),
            "```ts ignore-check\nsketch\n```\n".repeat(ignored),
            "```ts config-catalog\ninvalid content\n```\n```typescript\nignored language\n```\n"
        );
        write(root, "README.md", &text);
        let native = check_documentation_with_compiler(
            root,
            CompileMode::BuiltTypes,
            Some(&compiler_library()),
        )
        .unwrap();
        assert_eq!(
            (native.passed, native.stdout, native.stderr),
            oracle_command(root, "doc-typecheck", true),
            "{name}"
        );
    }
    let text = "```ts\nconst x = 1;\n```\n```ts public-api\nclass Api {}\n```\n```ts other\nignored\n```\n```ts persistence-catalog\ndata\n```\n```ts cordis-catalog\ndata\n```\n";
    assert_eq!(extract_blocks("doc.md", text).unwrap().len(), 4);
    let files = markdown_files(root).unwrap();
    assert_eq!(files, ["README.md"]);
    let block = extract_blocks("docs/x.md", "\n```ts\nx\n```\n").unwrap();
    let diagnostic = r"C:\repo\.doc-typecheck-z\block-0.ts(3,9) /tmp/block-20.ts(1,2)";
    assert_eq!(
        remap_block_paths(diagnostic, &block),
        "C:docs/x.md (block at line 2, +3:9) block-20.ts(1,2)"
    );
}

#[test]
fn compiler_declaration_projection_matches_source_for_public_api_and_jsdoc() {
    let temporary = fixture();
    let root = temporary.path();
    let source = r"/** Class documentation with 😀 and 中文. */
export default abstract class Example<T extends { value: string } = { value: string }> extends Base<T> implements Contract {
  /** Public field. */
  public readonly value: T = makeValue();
  private secret = 'secret';
  protected hidden(): void {}
  #privateValue = 1;
  static { initialize(); }
  /** Constructor documentation. */
  constructor(public visible: string, private hiddenParameter: number) { super(); }
  abstract run(input: T): Promise<string>;
  /** Accessor documentation. */
  get result(): string { return 'result'; }
  set result(value: string) { consume(value); }
  ['computed'](argument: number = 4): string { return `${argument}`; }
  [index: string]: unknown;
}
/** One declaration. */
export interface Model<T = string> { /** Member. */ value: T; }
export type Mapping<T> = { [K in keyof T]?: T[K] };
export enum Choice { One, Two = 'two' }
";
    write(root, "api.ts", source);
    let mut compiler = RepositoryCompiler::load(&compiler_library()).unwrap();
    let declarations = compiler
        .declarations(&root.join("api.ts").to_string_lossy(), source)
        .unwrap();
    let oracle = run_node(
        root,
        r"
import { readFileSync } from 'node:fs';
import * as oracle from './scripts/equiv-oracle.ts';
const code = readFileSync('api.ts', 'utf8');
const symbols = ['Example', 'Model', 'Mapping', 'Choice'];
console.log(JSON.stringify(symbols.map(symbol => ({ symbol, declaration: oracle.sourceDeclaration('api.ts',symbol), publicApi: oracle.sourcePublicApi('api.ts',symbol) }))));
",
        &Value::Null,
    );
    assert_eq!(serde_json::to_value(declarations).unwrap(), oracle);
    for code in [
        source,
        "const other = 1; export interface Escaped {}",
        "export default class {}",
        "namespace Inner { interface Nested {} }",
        "export enum State { A }",
        "/** doc */ interface Model {}",
    ] {
        let actual = compiler.block_symbol(code).unwrap();
        let oracle = run_node(
            root,
            r"
import * as oracle from './scripts/equiv-oracle.ts';
let input = ''; for await (const chunk of process.stdin) input += chunk;
console.log(JSON.stringify(oracle.blockSymbol(JSON.parse(input))));
",
            &json!(code),
        );
        assert_eq!(serde_json::to_value(actual).unwrap(), oracle);
    }
    for code in [
        "export default class X {}",
        " /** A. */ export interface X { /* ignored */ value: string; // comment\n }",
        "http://example.test\n// comment\n/** JSDoc \u{FEFF} retains text */ type A = string;",
        "export\u{FEFF}default\tinterface A {}",
    ] {
        let oracle = run_node(
            root,
            r"
import * as oracle from './scripts/equiv-oracle.ts';
let input = ''; for await (const chunk of process.stdin) input += chunk; const code = JSON.parse(input);
console.log(JSON.stringify([oracle.normalizeStructure(code), oracle.normalizeJSDoc(code), oracle.stripExport(code)]));
",
            &json!(code),
        );
        assert_eq!(
            json!([
                normalize_structure(code),
                normalize_jsdoc(code),
                strip_export(code)
            ]),
            oracle
        );
    }
}

#[test]
fn whole_type_equivalence_gate_matches_success_drift_duplicates_and_orphans() {
    let temporary = fixture();
    let root = temporary.path();
    write(
        root,
        "api.ts",
        "/** The model. */\nexport interface Model { /** Value. */ value: string; }\nexport class Api {\n  /** Read. */\n  read(): string { return 'value'; }\n  private hidden = 1;\n}\n",
    );
    let document = "# API\n\n```ts type-equiv\n/** The model. */\ninterface Model { /** Value. */ value: string; }\n```\n\n```ts public-api\ndeclare class Api {\n  /** Read. */\n  read(): string;\n}\n```\n";
    write(root, "docs/api.md", document);
    write(root, "docs/api.zh.md", document);
    let entries = json!([
        {"doc":"docs/api.md","symbol":"Model","source":"api.ts"},
        {"doc":"docs/api.md","symbol":"Api","source":"api.ts","projection":"public-api"}
    ]);
    write(
        root,
        "scripts/type-equiv.manifest.json",
        &json!({"entries": entries}).to_string(),
    );
    let native = verify_type_equiv_with_compiler(root, Some(&compiler_library())).unwrap();
    assert!(native.passed(), "{}", native.render());
    assert_eq!(native.verified, 2);
    assert_eq!(native.derivatives, 2);
    assert_type_equiv_command(root);
    write(
        root,
        "docs/api.md",
        &document.replace("/** Value. */", "/** Changed. */"),
    );
    assert_type_equiv_command(root);
    write(
        root,
        "docs/api.md",
        &format!("{document}\n```ts type-equiv\ninterface Model {{ value: string; }}\n```\n"),
    );
    let entries = json!([
        {"doc":"docs/api.md","symbol":"Model","source":"api.ts"},
        {"doc":"docs/api.md","symbol":"Model","source":"api.ts"},
        {"doc":"missing.md","symbol":"Missing","source":"missing.ts"},
        {"doc":"outside.md","symbol":"Outside","source":"api.ts"},
        {"doc":"docs/api.md","symbol":"Absent","source":"api.ts"}
    ]);
    write(root, "outside.md", "# Outside scope\n");
    write(
        root,
        "scripts/type-equiv.manifest.json",
        &json!({"entries": entries}).to_string(),
    );
    assert_type_equiv_command(root);
}

fn assert_type_equiv_command(root: &Path) {
    let native = verify_type_equiv_with_compiler(root, Some(&compiler_library())).unwrap();
    let source = oracle_command(root, "verify-type-equiv", false);
    let expected = if native.passed() {
        (true, native.render(), String::new())
    } else {
        (false, String::new(), native.render())
    };
    assert_eq!(expected, source);
}

#[test]
fn host_project_flattens_reference_cycles_and_imports_without_client_merges() {
    let temporary = fixture();
    let root = temporary.path();
    write(
        root,
        "tsconfig.json",
        r#"{"files":[],"references":[{"path":"./tsconfig.host.json"},{"path":"./tsconfig.client.json"}]}"#,
    );
    write(root, "tsconfig.client.json", r#"{"files":["client.ts"]}"#);
    write(
        root,
        "client.ts",
        "declare global { interface Model { client: never; } } export {};\n",
    );
    write(root, "empty.ts", "import './imported.ts'; export {};\n");
    write(
        root,
        "imported.ts",
        "export interface Imported { present: true; }\n",
    );
    write(
        root,
        "packages/runtime/tsconfig.json",
        r#"{
      "extends":"../../tsconfig.base.json", "include":["src/**/*.ts"],
      "references":[{"path":"../../tsconfig.host.json"}]
    }"#,
    );
    let mut native = TypeScriptProject::with_compiler(root, &compiler_library()).unwrap();
    let native_files = native.source_files().unwrap();
    assert!(
        native_files
            .iter()
            .any(|file| file.relative_path == "imported.ts")
    );
    assert!(
        !native_files
            .iter()
            .any(|file| file.relative_path == "client.ts")
    );
    assert_eq!(
        native.source_file("imported.ts").unwrap().text,
        "export interface Imported { present: true; }\n"
    );
    assert_eq!(
        native.source_file("client.ts").unwrap_err().to_string(),
        "TypeScript project did not load client.ts"
    );
    let oracle = project_oracle(root);
    assert_eq!(json!(native.root_names()), oracle["rootNames"]);
    assert_eq!(native.options(), &oracle["options"]);
    let expected_files = oracle["files"].as_array().unwrap();
    assert_eq!(native_files.len(), expected_files.len());
    for (actual, expected) in native_files.iter().zip(expected_files) {
        assert_eq!(json!(actual.file_name), expected["fileName"]);
        assert_eq!(json!(actual.relative_path), expected["relativePath"]);
        assert_eq!(
            json!(actual.is_declaration_file),
            expected["isDeclarationFile"]
        );
        assert!(
            expected["text"].as_str() == Some(actual.text.as_str()),
            "source contents differ for {}",
            actual.file_name
        );
    }
}

fn project_oracle(root: &Path) -> Value {
    let driver = r"
import { readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import ts from 'typescript';
let input = ''; for await (const chunk of process.stdin) input += chunk;
const source = readFileSync(JSON.parse(input), 'utf8');
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } }).outputText;
writeFileSync('scripts/ts-project-oracle.mjs', compiled);
try {
  const { TypeScriptProject } = await import(pathToFileURL(process.cwd() + '/scripts/ts-project-oracle.mjs').href);
  const project = new TypeScriptProject(process.cwd());
  console.log(JSON.stringify({ rootNames:project.program.getRootFileNames(),options:project.program.getCompilerOptions(),files:project.sourceFiles().map(sf=>({fileName:sf.fileName,relativePath:project.relativePath(sf),text:sf.text,isDeclarationFile:sf.isDeclarationFile})) }));
} catch (error) { console.log(JSON.stringify({error:error.message})); }
";
    run_node(
        root,
        driver,
        &json!(source_root().join("scripts/ts-project.ts")),
    )
}

#[test]
fn configuration_diagnostics_and_jsonc_are_owned_by_the_real_compiler() {
    let temporary = fixture();
    let root = temporary.path();
    let mut compiler = RepositoryCompiler::load(&compiler_library()).unwrap();
    let raw = compiler
        .read_config(&root.join("tsconfig.base.json"))
        .unwrap();
    assert_eq!(
        raw["compilerOptions"]["paths"]["@fixture/*"][0],
        "./packages/*/src/index.ts"
    );
    for content in [
        r#"{"compilerOptions":{"target":"not-real"},"files":["empty.ts"]}"#,
        r#"{"extends":"./missing.json","files":["empty.ts"]}"#,
        r#"{"compilerOptions": {"strict": } }"#,
    ] {
        write(root, "tsconfig.host.json", content);
        let error = TypeScriptProject::with_compiler(root, &compiler_library())
            .unwrap_err()
            .to_string();
        assert_eq!(json!({"error":error}), project_oracle(root));
    }
}

#[test]
fn invalid_equivalence_fences_preserve_source_errors() {
    let temporary = fixture();
    let root = temporary.path();
    let mut compiler = RepositoryCompiler::load(&compiler_library()).unwrap();
    for code in [
        "```ts type-equiv public-api\nclass Invalid {}\n```\n",
        "```ts type-equiv\ninterface Unclosed {}\n",
        "```ts public-api\nconst noDeclaration = 1;\n```\n",
        "```ts type-equiv\nnamespace OnlyNested { interface Inner {} }\n```\n",
    ] {
        write(root, "docs/invalid.md", code);
        let actual = extract_equiv_blocks(&mut compiler, "docs/invalid.md", code)
            .unwrap_err()
            .to_string();
        let expected = run_node(
            root,
            r"
import { extractEquivBlocks } from './scripts/equiv-oracle.ts';
try { extractEquivBlocks('docs/invalid.md'); console.log(JSON.stringify({ok:true})); }
catch (error) { console.log(JSON.stringify({error:error.message})); }
",
            &Value::Null,
        );
        assert_eq!(json!({"error":actual}), expected);
    }
}

#[test]
fn built_binaries_execute_the_source_contract_with_portable_arguments() {
    let temporary = fixture();
    let root = temporary.path();
    write(
        root,
        "README.md",
        "```ts\nimport { greet } from '@fixture/runtime';\ngreet('hello');\n```\n",
    );
    write(root, "scripts/type-equiv.manifest.json", "{\"entries\":[]}");
    for (binary, source, arguments) in [
        (
            env!("CARGO_BIN_EXE_doc-typecheck"),
            "doc-typecheck",
            vec!["--use-build-output"],
        ),
        (
            env!("CARGO_BIN_EXE_verify-type-equiv"),
            "verify-type-equiv",
            vec![],
        ),
    ] {
        let actual = Command::new(binary)
            .arg("--root")
            .arg(root)
            .args(arguments)
            .env_remove("SEEKDEEP_TYPESCRIPT_LIBRARY")
            .env_remove("SEEKDEEP_DOC_TYPECHECK_USE_BUILD_OUTPUT")
            .env_remove("DSH_DOC_TYPECHECK_USE_BUILD_OUTPUT")
            .output()
            .unwrap();
        assert_eq!(
            (
                actual.status.success(),
                String::from_utf8(actual.stdout).unwrap(),
                String::from_utf8(actual.stderr).unwrap()
            ),
            oracle_command(root, source, true)
        );
    }
}

#[test]
fn full_pinned_declaration_corpus_matches_the_source_gate() {
    let root = source_root();
    let native = verify_type_equiv_with_compiler(&root, Some(&compiler_library())).unwrap();
    assert!(
        native.verified > 20,
        "the whole source corpus must be exercised"
    );
    let expected = oracle_command(&root, "verify-type-equiv", false);
    let actual = if native.passed() {
        (true, native.render(), String::new())
    } else {
        (false, String::new(), native.render())
    };
    assert_eq!(actual, expected);
}

#[test]
fn native_commands_resolve_the_rust_build_support_compiler_install() {
    let temporary = fixture();
    let root = temporary.path();
    std::fs::remove_file(root.join("node_modules")).unwrap();
    std::fs::create_dir_all(root.join("support/browser-dependencies")).unwrap();
    let support_modules = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../support/browser-dependencies/node_modules")
        .canonicalize()
        .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        &support_modules,
        root.join("support/browser-dependencies/node_modules"),
    )
    .unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(
        &support_modules,
        root.join("support/browser-dependencies/node_modules"),
    )
    .unwrap();
    write(
        root,
        "README.md",
        "```ts\nimport { greet } from '@fixture/runtime';\ngreet('hello');\n```\n",
    );
    for arguments in [vec!["--use-build-output"], Vec::new()] {
        let output = Command::new(env!("CARGO_BIN_EXE_doc-typecheck"))
            .arg("--root")
            .arg(root)
            .args(arguments)
            .env_remove("SEEKDEEP_TYPESCRIPT_LIBRARY")
            .env_remove("SEEKDEEP_DOC_TYPECHECK_USE_BUILD_OUTPUT")
            .env_remove("DSH_DOC_TYPECHECK_USE_BUILD_OUTPUT")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 block(s) compiled"));
    }
}

fn declaration_files(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut result = walkdir::WalkDir::new(root.join("packages"))
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    result.sort();
    result
}
