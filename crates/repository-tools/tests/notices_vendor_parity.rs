//! Source-differential and executable fixture coverage for notices and vendor migration.

use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use indexmap::IndexMap;
use seekdeep_repository_tools::{
    rescope_vendor::{self, Mode},
    third_party_notices::{self as notices, Manifest},
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../deepseek-harness")
}

fn normalize(text: &str) -> String {
    text.replace("@deepseek-ai", "@seekdeep-ai")
        .replace("DeepSeek Harness", "SeekDeep Harness")
        .replace("`dsh`", "`seekdeep`")
        .replace("deepseek-harness-sdk", "seekdeep-harness-sdk")
        .replace("dsh-packed-consumer", "seekdeep-packed-consumer")
}

fn oracle(script: &str, root: &Path, operation: &str, input: Value) -> Value {
    let temporary = TempDir::new().unwrap();
    let program = r#"
import { readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
const request = JSON.parse(readFileSync(0, 'utf8'));
const require = createRequire(pathToFileURL(request.source));
let source = readFileSync(request.source, 'utf8');
source = source.replace("const root = resolve(import.meta.dirname, '..')", 'const root = ' + JSON.stringify(request.root));
if (request.operation.startsWith('rescope')) source = source.replaceAll('@deepseek-ai', '@seekdeep-ai').replaceAll('DeepSeek Harness', 'SeekDeep Harness').replaceAll('dsh-packed-consumer', 'seekdeep-packed-consumer');
for (const dependency of ['js-yaml', 'smol-toml', 'spdx-expression-parse']) {
  if (source.includes("from '" + dependency + "'")) source = source.replace("from '" + dependency + "'", "from '" + pathToFileURL(require.resolve(dependency)).href + "'");
}
for (const name of ['RENAMES', 'GENERIC_SKIPS', 'POSTCONDITIONS', 'EXACT_EDITS']) source = source.replace('const ' + name + ':', 'export const ' + name + ':');
for (const name of ['normalizeRepo', 'rewrite', 'patterns', 'excluded', 'main']) source = source.replace('function ' + name + '(', 'export function ' + name + '(');
writeFileSync(request.module, source);
const module = await import(pathToFileURL(request.module).href);
const run = (call) => {
  try {
    let result;
    switch (call.operation) {
      case 'licenses': result = call.input.map(value => module.isPermissive(value)); break;
      case 'python': result = module.parsePyprojectRequirements(call.input); break;
      case 'python-collect': result = module.collectPythonDependencies(call.input); break;
      case 'claude': result = module.claudeDistributionFromManifest(call.input); break;
      case 'owner': result = call.input.map(value => module.isOwnerAuthorizedRuntime(value)); break;
      case 'patterns': result = module.manifestPatterns(call.input); break;
      case 'tier': result = [...module.tierExternalDeps(new Map(Object.entries(call.input.manifests)), new Set(call.input.names))]; break;
      case 'virtual': result = module.virtualManifest(call.input.store, call.input.name); break;
      case 'vendored': result = module.parseVendoredRows(call.input); break;
      case 'repositories': result = call.input.map(value => module.normalizeRepo(value ?? undefined) ?? null); break;
      case 'render': result = module.render(); break;
      case 'rescope-rewrite': result = call.input.map(({text, file, reverse}) => module.rewrite(text, file, module.patterns(reverse))); break;
      case 'rescope-excluded': result = call.input.map(file => module.excluded(file)); break;
      case 'rescope-policy': result = {renames: module.RENAMES, genericSkips: module.GENERIC_SKIPS, postconditions: module.POSTCONDITIONS, exactEdits: module.EXACT_EDITS}; break;
      case 'rescope-run': {
        let stdout = '', stderr = '';
        console.log = (...args) => { stdout += args.join(' ') + '\n'; };
        console.error = (...args) => { stderr += args.join(' ') + '\n'; };
        process.argv = ['node', request.module, ...call.input];
        process.exitCode = 0;
        module.main();
        result = {stdout, stderr, success: process.exitCode === 0};
        process.exitCode = 0;
        break;
      }
      default: throw Error('unknown operation ' + call.operation);
    }
    return {ok: result ?? null};
  } catch (error) { return {error: error.message}; }
};
const result = request.operation === 'batch' ? request.input.map(run) : run(request);
process.stdout.write(JSON.stringify(result));
"#;
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", program])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut request = json!({"source":source_root().join("scripts").join(script), "root":root, "module":temporary.path().join("oracle.mts"), "operation":operation});
    request["input"] = input;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&request).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "oracle JSON: {error}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn notices_oracle(operation: &str, input: Value) -> Value {
    oracle(
        "gen-third-party-notices.ts",
        &source_root(),
        operation,
        input,
    )
}

fn result_value<T: serde::Serialize>(result: anyhow::Result<T>) -> Value {
    match result {
        Ok(value) => json!({"ok":value}),
        Err(error) => json!({"error":error.to_string()}),
    }
}

fn write(root: &Path, file: &str, contents: &str) {
    let path = root.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn spdx_policy_matches_the_installed_parser_catalog_and_expression_grammar() {
    let policy: Value =
        serde_json::from_str(include_str!("../src/third_party_notices/policy.json")).unwrap();
    let mut expressions = [
        "MIT",
        "ISC",
        "Apache-2.0",
        "MIT / Apache-2.0",
        "MIT AND ISC",
        "MIT OR (GPL-3.0-only AND GPL-2.0-only)",
        "(MIT OR Apache-2.0) AND ISC",
        "MIT)",
        "((MIT",
        "MIT OR OR GPL-3.0-only",
        "MIT+",
        "MIT +",
        "MIT OR Unknown-license",
        "MIT and ISC",
        "MITorISC",
        "\tMIT\u{feff}",
        "MIT\tOR ISC",
        "MIT\nOR ISC",
        "MIT OR LicenseRef-local",
        "DocumentRef-document:LicenseRef-license OR MIT",
        "LicenseRef-foo WITH Classpath-exception-2.0 OR MIT",
        "MIT WITH Unknown-exception OR ISC",
        "MIT WITH Classpath-exception-2.0 OR ISC",
        "LicenseRef-foo+ OR MIT",
        "LicenseRef- OR MIT",
        "",
        "MIT // ISC",
        "MIT\u{0085}",
        "(MIT OR Apache-2.0) WITH Classpath-exception-2.0",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for license in policy["licenses"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
    {
        expressions.extend([
            license.to_owned(),
            format!("{license} OR MIT"),
            format!("{license} AND MIT"),
            format!("({license}+ OR ISC)"),
        ]);
    }
    for exception in policy["exceptions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
    {
        expressions.push(format!("MIT WITH {exception} OR ISC"));
    }
    let expected = notices_oracle("licenses", json!(expressions));
    eprintln!(
        "notices SPDX differential: {} expressions",
        expressions.len()
    );
    let actual = expressions
        .iter()
        .map(|expression| notices::is_permissive(expression))
        .collect::<Vec<_>>();
    for (index, result) in actual.iter().enumerate() {
        assert_eq!(
            json!(result),
            expected["ok"][index],
            "{}",
            expressions[index]
        );
    }
}

#[test]
fn requirement_tables_and_failure_messages_match_the_toml_source() {
    let cases = [
        "[build-system]\nrequires=['hatchling>=1.24.0']\n[project]\nname='local'\ndependencies=['pydantic>=2','httpx[http2]', 'tomli ; python_version < \"3.11\"']\n[project.optional-dependencies]\nz=['requests']\na=['click']\n[dependency-groups]\nall=[{include-group='z'}]\nz=['pytest']\na=['sphinx']\n",
        "[project] # header\ndependencies=[\n 'pydantic', # ]\n # 'ignored'\n 'tomli',\n]\n[dependency-groups]\n\"test.docs\"=['pytest']\n",
        "[project]\ndependencies = ['hello @ https://example.test/pkg.whl', 'Hatchling >= 1.0']\n",
        "[project]\ndependencies=['!!broken']\n",
        "[project]\ndependencies='pytest'\n",
        "[dependency-groups]\ntest=[{unknown='pytest'}]\n",
        "[dependency-groups]\ntest=[{include-group='base', extra='bad'}]\n",
        "[project]\nname=10\n",
        "project=[]\n",
        "[project]\noptional-dependencies=[]\n",
        "[project]\ndependencies=[false]\n",
        "[project.optional-dependencies]\ntest=[{include-group='base'}]\n",
        "[build-system]\nrequires=['']\n",
        "[project]\ndependencies=['a\\b']\n",
        "[project]\ndependencies=[]\n",
        "[project]\ndependencies=['\u{feff}pydantic']\n",
        "[project]\ndependencies=['\u{0085}pydantic']\n",
        "[dependency-groups]\n\"10\"=['ten']\n\"2\"=['two']\n\"01\"=['one']\nz=['last']\n",
    ];
    let input = cases
        .iter()
        .map(|text| json!({"operation":"python","input":text}))
        .collect::<Vec<_>>();
    let expected = notices_oracle("batch", json!(input));
    for (index, text) in cases.iter().enumerate() {
        assert_eq!(
            result_value(notices::parse_pyproject_requirements(text)),
            expected[index],
            "{text}"
        );
    }
    let pyprojects = vec![
        "[project]\nname='local-pkg'\ndependencies=['pydantic']\n".to_owned(),
        "[project]\nname='other'\ndependencies=['LOCAL_Pkg','pytest']\n".to_owned(),
    ];
    assert_eq!(
        normalize(
            &result_value(notices::collect_python_dependencies(&pyprojects, None)).to_string()
        ),
        normalize(&notices_oracle("python-collect", json!(pyprojects)).to_string())
    );
    let unrelated =
        vec!["[project]\nname='local-pkg'\ndependencies=['local-unrelated']\n".to_owned()];
    assert_eq!(
        result_value(notices::collect_python_dependencies(&unrelated, None)),
        notices_oracle("python-collect", json!(unrelated))
    );
}

#[test]
fn workspace_tiering_and_repo_normalization_preserve_declaration_semantics() {
    let input = json!({"package.json":{"dependencies":{"root":"1"},"devDependencies":{"shared":"1"}},"packages/test-support/client-runtime/package.json":{"dependencies":{"fixture":"1"}},"native/landlock-run/package.json":{"dependencies":{"native-build":"1"}},"website/package.json":{"dependencies":{"site":"1"}},"examples/demo/package.json":{"dependencies":{"demo":"1"}},"packages/plugin/foo/package.json":{"name":"local-foo","dependencies":{"runtime":"1","local-bar":"1","missing-workspace":"workspace:*","shared":"1"},"optionalDependencies":{"optional":"1"},"peerDependencies":{"peer":"1"}},"apps/cli/package.json":{"name":"local-bar","dependencies":{"cli":"1"}}});
    let manifests: IndexMap<String, Manifest> = serde_json::from_value(input.clone()).unwrap();
    let names = HashSet::from(["local-foo".to_owned(), "local-bar".to_owned()]);
    let actual = notices::tier_external_deps(&manifests, &names)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(
        json!(actual),
        notices_oracle("tier", json!({"manifests":input,"names":names}))["ok"]
    );
    let members = vec![
        "packages/*/*".to_owned(),
        "tools/*".to_owned(),
        "native/landlock-run".to_owned(),
    ];
    assert_eq!(
        json!(notices::manifest_patterns(&members)),
        notices_oracle("patterns", json!(members))["ok"]
    );
    let repositories = vec![
        None,
        Some(""),
        Some("git+ssh://git@github.com/a/b.git"),
        Some("git+https://github.com/a/b.git"),
        Some("git://example.com/a.git"),
        Some("github:a/b"),
        Some("a/b"),
        Some("https://example.com/a.git?q=1"),
        Some("httpish"),
        Some("git+github:a/b.git"),
    ];
    assert_eq!(
        json!(
            repositories
                .iter()
                .map(|value| notices::normalize_repo(*value))
                .collect::<Vec<_>>()
        ),
        notices_oracle("repositories", json!(repositories))["ok"]
    );
}

#[test]
fn claude_identity_payload_order_and_rejection_boundaries_match_source() {
    let sdk = notices::CLAUDE_AGENT_SDK_PACKAGE;
    let cases = vec![
        json!({"name":sdk,"version":"9.8.7","claudeCodeVersion":"6.5.4","optionalDependencies":{format!("{sdk}-linux-x64"):"9.8.7",format!("{sdk}-darwin-arm64"):"9.8.7"}}),
        json!({}),
        json!({"name":"@anthropic-ai/unrelated"}),
        json!({"name":sdk}),
        json!({"name":sdk,"version":"1"}),
        json!({"name":sdk,"version":"1","claudeCodeVersion":"1"}),
        json!({"name":sdk,"version":"1","claudeCodeVersion":"1","optionalDependencies":{"@anthropic-ai/unrelated":"1"}}),
        json!({"name":sdk,"version":"1","claudeCodeVersion":"1","optionalDependencies":{format!("{sdk}-future"):""}}),
    ];
    let expected = notices_oracle(
        "batch",
        json!(
            cases
                .iter()
                .map(|input| json!({"operation":"claude","input":input}))
                .collect::<Vec<_>>()
        ),
    );
    for (index, case) in cases.into_iter().enumerate() {
        let manifest: Manifest = serde_json::from_value(case).unwrap();
        assert_eq!(
            result_value(notices::claude_distribution_from_manifest(&manifest)),
            expected[index]
        );
    }
    let names = [
        sdk,
        "@anthropic-ai/claude-agent-sdk-linux-x64",
        "@anthropic-ai/unrelated",
    ];
    assert_eq!(
        json!(names.map(notices::is_owner_authorized_runtime)),
        notices_oracle("owner", json!(names))["ok"]
    );
}

#[test]
fn pnpm_virtual_store_prefix_and_truncated_content_resolution_match_source() {
    let fixture = TempDir::new().unwrap();
    for (directory, name, version) in [
        ("@scope+first@1", "@scope/first", "1"),
        ("@scope+long_abc123", "@scope/long", "2"),
        ("other@1", "other", "1"),
    ] {
        write(
            fixture.path(),
            &format!("{directory}/node_modules/{name}/package.json"),
            &json!({"name":name,"version":version,"license":"MIT"}).to_string(),
        );
    }
    for name in ["@scope/first", "@scope/long", "@scope/missing"] {
        assert_eq!(
            result_value(notices::virtual_manifest(fixture.path(), name)),
            notices_oracle("virtual", json!({"store":fixture.path(),"name":name}))
        );
    }
    fs::create_dir_all(fixture.path().join("@scope+broken@1")).unwrap();
    assert!(notices::virtual_manifest(fixture.path(), "@scope/broken").is_err());
}

#[test]
fn pinned_notices_corpus_renders_identically_after_product_identity_changes() {
    let root = source_root();
    let table = fs::read_to_string(root.join("vendor/README.md")).unwrap();
    assert_eq!(
        json!(notices::parse_vendored_rows(&table)),
        notices_oracle("vendored", json!(table))["ok"]
    );
    for whitespace in ['\u{0085}', '\u{feff}', '\u{00a0}', '\r'] {
        let table = format!("| `v/` | `n` | `u` | 1 | https://x{whitespace}y | `a` |\n");
        assert_eq!(
            json!(notices::parse_vendored_rows(&table)),
            notices_oracle("vendored", json!(table))["ok"]
        );
    }
    let expected = notices_oracle("render", Value::Null);
    let actual = notices::render(&root).unwrap();
    assert_eq!(
        normalize(&actual),
        normalize(expected["ok"].as_str().unwrap())
    );
}

#[test]
fn rescope_policy_and_token_boundaries_match_source() {
    let expected = oracle(
        "rescope-vendor.ts",
        &source_root(),
        "rescope-policy",
        Value::Null,
    );
    let mut policy: Value =
        serde_json::from_str(include_str!("../src/rescope_vendor/policy.json")).unwrap();
    policy.as_object_mut().unwrap().shift_remove("sourceCommit");
    assert_eq!(policy, expected["ok"]);
    let texts = [
        "import x from 'cordis'; const y = \"cosmokit/lib\"; `schemastery`\n",
        "'cordis' \"cordis' 'cordis\" 'cordis/sub path' 'cordis.yml' 'cordis:' 'cordiverse/cordis'\n",
        "name: cordis # comment\n  - name:\t@cordisjs/plugin-hmr\r\n",
        "`cordis` prose\n```shell\nimport 'cordis'\n```\n~~~js\n'cordis'\n~~~\n",
        "import 'cordis/工具'; 'schemastery/x'; name: cordis\n",
        "\u{feff}name: cordis\nname: cordis\r\n",
        "const x = 'cordis/\u{0085}foo'; const y = 'cordis/\u{a0}foo';\n",
        "name: @seekdeep-ai/cordis\n'@seekdeep-ai/cordis/client'\n",
        "other\u{2028}name: cordis\u{2029}name: cosmokit\rname: schemastery\n",
    ];
    let mut cases = Vec::new();
    for text in texts {
        for file in [
            "src/index.ts",
            "README.md",
            "docs/guide.md",
            "vendor/schemastery/src/index.ts",
            "apps/cli/config/agent-presets/cordis/agent.cordis.yml",
        ] {
            for reverse in [false, true] {
                cases.push(json!({"text":text,"file":file,"reverse":reverse}));
            }
        }
    }
    let expected = oracle(
        "rescope-vendor.ts",
        &source_root(),
        "rescope-rewrite",
        json!(cases),
    );
    eprintln!("rescope token differential: {} cases", cases.len());
    for (index, case) in cases.iter().enumerate() {
        let (text, lines) = rescope_vendor::rewrite(
            case["text"].as_str().unwrap(),
            case["file"].as_str().unwrap(),
            case["reverse"].as_bool().unwrap(),
        );
        assert_eq!(
            json!({"text":text,"lines":lines}),
            expected["ok"][index],
            "{case}"
        );
    }
    let files = [
        "scripts/rescope-vendor.ts",
        ".agents/notes/proposed/x.md",
        "scripts/snapshots/a.ts",
        "docs/rescope.md",
        "docs/rescope.zh.md",
        "docs/x.i18n.yaml",
        "pnpm-lock.yaml",
        "vendor/cordis/README.md",
        "vendor/cordis/LICENSE",
        "vendor/cordis/src/index.ts",
        "vendor/cordis/README.zh.md",
        "a.ts",
        "a.rs",
        "x.tpl",
    ];
    assert_eq!(
        json!(files.map(rescope_vendor::excluded)),
        oracle(
            "rescope-vendor.ts",
            &source_root(),
            "rescope-excluded",
            json!(files)
        )["ok"]
    );
}

fn git_fixture() -> TempDir {
    let fixture = TempDir::new().unwrap();
    let policy = rescope_vendor::policy();
    let paths = policy
        .exact_edits
        .iter()
        .map(|edit| edit.file.as_str())
        .chain(
            policy
                .postconditions
                .iter()
                .map(|check| check.file.as_str()),
        )
        .collect::<HashSet<_>>();
    for file in paths {
        write(
            fixture.path(),
            file,
            &normalize(&fs::read_to_string(source_root().join(file)).unwrap()),
        );
    }
    write(
        fixture.path(),
        "test-fixture.ts",
        "import { Context } from '@seekdeep-ai/cordis'\nconst id = '@seekdeep-ai/cosmokit/path'\n",
    );
    write(
        fixture.path(),
        "fixture.md",
        "Prose `cordis`\n```ts\nimport '@seekdeep-ai/cordis'\n```\n",
    );
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(fixture.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["add", "--force", "."])
            .current_dir(fixture.path())
            .status()
            .unwrap()
            .success()
    );
    fixture
}

fn compare_trees(left: &Path, right: &Path) {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(left)
        .output()
        .unwrap();
    for file in String::from_utf8(output.stdout)
        .unwrap()
        .split('\0')
        .filter(|file| !file.is_empty())
    {
        assert_eq!(
            fs::read(left.join(file)).unwrap(),
            fs::read(right.join(file)).unwrap(),
            "{file}"
        );
    }
}

#[test]
fn full_rescope_command_matches_forward_reverse_dry_and_idempotent_checks() {
    let rust = git_fixture();
    let source = git_fixture();
    for (mode, reverse) in [
        (Mode::Check, false),
        (Mode::Dry, true),
        (Mode::Apply, true),
        (Mode::Check, true),
        (Mode::Apply, true),
        (Mode::Dry, false),
        (Mode::Apply, false),
        (Mode::Check, false),
        (Mode::Apply, false),
    ] {
        let args = match mode {
            Mode::Dry => Vec::new(),
            Mode::Apply => vec!["--apply"],
            Mode::Check => vec!["--check"],
        }
        .into_iter()
        .chain(reverse.then_some("--reverse"))
        .collect::<Vec<_>>();
        let expected = oracle(
            "rescope-vendor.ts",
            source.path(),
            "rescope-run",
            json!(args),
        );
        let actual = rescope_vendor::run(rust.path(), mode, reverse).unwrap();
        assert_eq!(
            json!({"stdout":actual.stdout,"stderr":actual.stderr,"success":actual.failures.is_empty()}),
            expected["ok"],
            "{mode:?} reverse={reverse}"
        );
        compare_trees(rust.path(), source.path());
    }
}

#[test]
fn invalid_exact_edits_prevent_every_write_and_postconditions_reject_missing_sites() {
    let rust = git_fixture();
    let source = git_fixture();
    let file = "packages/client/tsdown.client.ts";
    let edit = &rescope_vendor::policy()
        .exact_edits
        .iter()
        .find(|edit| edit.id == "client-purity-vendored-libraries")
        .unwrap();
    for root in [rust.path(), source.path()] {
        let text = fs::read_to_string(root.join(file)).unwrap();
        write(
            root,
            file,
            &text.replace(&edit.replace, &format!("{}{}", edit.replace, edit.replace)),
        );
    }
    let before = fs::read(rust.path().join(file)).unwrap();
    let expected = oracle(
        "rescope-vendor.ts",
        source.path(),
        "rescope-run",
        json!(["--apply"]),
    );
    let actual = rescope_vendor::run(rust.path(), Mode::Apply, false).unwrap();
    assert_eq!(
        json!({"stdout":actual.stdout,"stderr":actual.stderr,"success":actual.failures.is_empty()}),
        expected["ok"]
    );
    assert_eq!(fs::read(rust.path().join(file)).unwrap(), before);
    compare_trees(rust.path(), source.path());
    let fixture = git_fixture();
    write(
        fixture.path(),
        "vendor/cordis/package.json",
        "{\"name\":\"cordis\"}\n",
    );
    let report = rescope_vendor::run(fixture.path(), Mode::Check, false).unwrap();
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.starts_with("postcondition: vendor/cordis/package.json"))
    );
    assert!(report.failures.iter().any(|failure| failure
        == "residue: vendor/cordis/package.json still carries a pre-rescope name token"));
}

#[test]
fn embedded_rescope_policy_is_preserved_by_forward_and_reverse_migrations() {
    let fixture = git_fixture();
    let file = "crates/repository-tools/src/rescope_vendor/policy.json";
    let policy = include_str!("../src/rescope_vendor/policy.json");
    write(fixture.path(), file, policy);
    assert!(
        Command::new("git")
            .args(["add", "--force", file])
            .current_dir(fixture.path())
            .status()
            .unwrap()
            .success()
    );
    for (mode, reverse) in [
        (Mode::Check, false),
        (Mode::Apply, true),
        (Mode::Check, true),
        (Mode::Apply, false),
        (Mode::Check, false),
    ] {
        let report = rescope_vendor::run(fixture.path(), mode, reverse).unwrap();
        assert!(report.failures.is_empty(), "{}", report.stderr);
        assert_eq!(
            fs::read_to_string(fixture.path().join(file)).unwrap(),
            policy
        );
    }
}

fn notice_fixture() -> TempDir {
    let fixture = TempDir::new().unwrap();
    write(
        fixture.path(),
        "scripts/build-exe-for-python-sdk.ts",
        "const packager = '@yao-pkg/pkg'\n",
    );
    write(
        fixture.path(),
        "pnpm-workspace.yaml",
        "packages:\n  - packages/*/*\n  - vendor/*\npatchedDependencies:\n  runtime@1: patches/runtime.patch\n  second@2: patches/second.patch\n",
    );
    write(
        fixture.path(),
        "package.json",
        &json!({"devDependencies":{"dev-tool":"1"}}).to_string(),
    );
    for index in 0..105 {
        let manifest = if index == 0 {
            json!({"name":"fixture-local","dependencies":{"@scope/runtime":"1",notices::CLAUDE_AGENT_SDK_PACKAGE:"9.8.7"}})
        } else {
            json!({"name":format!("fixture-local-{index}")})
        };
        write(
            fixture.path(),
            &format!("packages/demo/p{index:03}/package.json"),
            &manifest.to_string(),
        );
    }
    write(
        fixture.path(),
        "vendor/cordis/package.json",
        &json!({"name":"@seekdeep-ai/cordis","license":"MIT"}).to_string(),
    );
    write(
        fixture.path(),
        "vendor/README.md",
        "| `cordis/` | `@seekdeep-ai/cordis` | `cordis` | 4.0.0 | https://github.com/cordiverse/cordis | `abc123` |\n",
    );
    write(
        fixture.path(),
        "python/sdk/pyproject.toml",
        "[build-system]\nrequires=['hatchling']\n[project]\nname='seekdeep-harness-sdk'\ndependencies=['pydantic']\n[dependency-groups]\ntest=['pytest']\n",
    );
    for (name, license, repository) in [
        ("tsx", "MIT", "github:privatenumber/tsx"),
        (
            "@scope/runtime",
            "MIT OR Apache-2.0",
            "git+ssh://git@github.com/example/runtime.git",
        ),
        ("dev-tool", "GPL-3.0-only", "example/dev-tool"),
    ] {
        write(
            fixture.path(),
            &format!("node_modules/{name}/package.json"),
            &json!({"name":name,"version":"1","license":license,"repository":repository})
                .to_string(),
        );
    }
    let sdk = notices::CLAUDE_AGENT_SDK_PACKAGE;
    write(fixture.path(), &format!("node_modules/{sdk}/package.json"), &json!({"name":sdk,"version":"9.8.7","license":"SEE LICENSE IN README.md","repository":"https://github.com/anthropics/claude-agent-sdk","claudeCodeVersion":"6.5.4","optionalDependencies":{format!("{sdk}-linux-x64"):"9.8.7",format!("{sdk}-darwin-arm64"):"9.8.7"}}).to_string());
    write(fixture.path(), &format!("node_modules/{sdk}-darwin-arm64/package.json"), &json!({"name":format!("{sdk}-darwin-arm64"),"version":"9.8.7","license":"SEE LICENSE IN LICENSE.md"}).to_string());
    fixture
}

#[test]
fn installed_fixture_notices_and_cli_write_check_staleness_are_complete() {
    let fixture = notice_fixture();
    let expected = oracle(
        "gen-third-party-notices.ts",
        fixture.path(),
        "render",
        Value::Null,
    );
    let actual = notices::render(fixture.path()).unwrap();
    assert_eq!(
        normalize(&actual),
        normalize(expected["ok"].as_str().unwrap())
    );
    assert!(actual.contains("Official Claude Code platform payloads"));
    assert!(actual.contains("`dev-tool` (GPL-3.0-only) runs only as development tooling"));
    let binary = env!("CARGO_BIN_EXE_gen-third-party-notices");
    let invoke = |check| {
        let mut command = Command::new(binary);
        command.arg("--root").arg(fixture.path());
        if check {
            command.arg("--check");
        }
        command.output().unwrap()
    };
    let missing = invoke(true);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("THIRD_PARTY_NOTICES.md is stale"));
    assert!(!fixture.path().join("THIRD_PARTY_NOTICES.md").exists());
    assert!(invoke(false).status.success());
    assert_eq!(
        fs::read_to_string(fixture.path().join("THIRD_PARTY_NOTICES.md")).unwrap(),
        actual
    );
    let checked = invoke(true);
    assert!(checked.status.success());
    assert_eq!(
        String::from_utf8(checked.stdout).unwrap(),
        "gen-third-party-notices: THIRD_PARTY_NOTICES.md is up to date.\n"
    );
    write(fixture.path(), "THIRD_PARTY_NOTICES.md", "stale\n");
    assert!(!invoke(true).status.success());
    assert_eq!(
        fs::read_to_string(fixture.path().join("THIRD_PARTY_NOTICES.md")).unwrap(),
        "stale\n"
    );
    write(
        fixture.path(),
        "node_modules/dev-tool/package.json",
        &json!({"name":"dev-tool","license":"{{PYTHON_ROWS}}","repository":"example/dev-tool"})
            .to_string(),
    );
    let expected = oracle(
        "gen-third-party-notices.ts",
        fixture.path(),
        "render",
        Value::Null,
    );
    assert_eq!(
        normalize(&notices::render(fixture.path()).unwrap()),
        normalize(expected["ok"].as_str().unwrap())
    );
    write(
        fixture.path(),
        "pnpm-workspace.yaml",
        "packages:\n  - ['packages/*/*']\n  - vendor/*\npatchedDependencies:\n  '10': 10.0\n  '2': [a, null, b]\n  '01': {local: true}\n  x: null\n",
    );
    let expected = oracle(
        "gen-third-party-notices.ts",
        fixture.path(),
        "render",
        Value::Null,
    );
    assert_eq!(
        normalize(&notices::render(fixture.path()).unwrap()),
        normalize(expected["ok"].as_str().unwrap())
    );
}

#[test]
fn notices_fail_closed_on_discovery_metadata_licenses_and_payload_mismatch() {
    let sdk = notices::CLAUDE_AGENT_SDK_PACKAGE;
    let mutations = vec![
        ("node_modules/@scope/runtime/package.json".to_owned(), json!({"name":"@scope/runtime","license":"GPL-3.0-only","repository":"example/runtime"}).to_string()),
        ("node_modules/@scope/runtime/package.json".to_owned(), json!({"name":"@scope/runtime","repository":"example/runtime"}).to_string()),
        ("node_modules/@scope/runtime/package.json".to_owned(), json!({"name":"@scope/runtime","license":"MIT"}).to_string()),
        (format!("node_modules/{sdk}-darwin-arm64/package.json"), json!({"name":format!("{sdk}-darwin-arm64"),"version":"wrong","license":"SEE LICENSE IN LICENSE.md"}).to_string()),
        (format!("node_modules/{sdk}/package.json"), json!({"name":sdk,"version":"9.8.7","claudeCodeVersion":"6.5.4","license":"SEE LICENSE IN README.md","repository":"example/sdk","optionalDependencies":{"@anthropic-ai/unrelated":"1"}}).to_string()),
        ("vendor/README.md".to_owned(), "changed table shape\n".to_owned()),
        ("vendor/cordis/package.json".to_owned(), json!({"name":"@seekdeep-ai/cordis","license":"Apache-2.0"}).to_string()),
        ("python/sdk/pyproject.toml".to_owned(), "[project]\ndependencies=['unknown-distribution']\n".to_owned()),
        ("pnpm-workspace.yaml".to_owned(), "packages: []\n".to_owned()),
        ("pnpm-workspace.yaml".to_owned(), "packages: ['tools/*']\n".to_owned()),
        ("scripts/build-exe-for-python-sdk.ts".to_owned(), "const packager = 'different-package'\n".to_owned()),
    ];
    for (file, content) in mutations {
        let fixture = notice_fixture();
        write(fixture.path(), &file, &content);
        let expected = oracle(
            "gen-third-party-notices.ts",
            fixture.path(),
            "render",
            Value::Null,
        );
        let actual = result_value(notices::render(fixture.path()));
        assert!(actual.get("error").is_some(), "{file}");
        assert_eq!(
            normalize(&actual.to_string()),
            normalize(&expected.to_string()),
            "{file}"
        );
    }
    let fixture = notice_fixture();
    fs::remove_dir_all(
        fixture
            .path()
            .join(format!("node_modules/{sdk}-darwin-arm64")),
    )
    .unwrap();
    assert_eq!(
        result_value(notices::render(fixture.path())),
        oracle(
            "gen-third-party-notices.ts",
            fixture.path(),
            "render",
            Value::Null
        )
    );
}

#[test]
fn native_executable_pipeline_does_not_disclose_a_removed_npm_packager() {
    let fixture = notice_fixture();
    fs::remove_file(fixture.path().join("scripts/build-exe-for-python-sdk.ts")).unwrap();
    write(
        fixture.path(),
        "crates/python-release/src/executable/pipeline.rs",
        "let command = Command::new(\"cargo\");\n",
    );
    let collection = notices::collect(fixture.path()).unwrap();
    assert!(collection.build_time_tools.is_empty());
    assert!(
        !notices::render_collection(&collection)
            .unwrap()
            .contains("@yao-pkg/pkg")
    );
    write(
        fixture.path(),
        "crates/python-release/src/executable/pipeline.rs",
        "let command = Command::new(\"unknown\");\n",
    );
    assert!(notices::collect(fixture.path()).is_err());
}

#[test]
fn rescope_cli_dispatches_reverse_and_apply_precedence_against_a_tracked_fixture() {
    let fixture = git_fixture();
    let binary = env!("CARGO_BIN_EXE_rescope-vendor");
    for arguments in [
        vec!["--check"],
        vec!["--apply", "--reverse"],
        vec!["--check", "--reverse"],
        vec!["--check", "--apply"],
        vec!["--check"],
    ] {
        let result = Command::new(binary)
            .arg("--root")
            .arg(fixture.path())
            .args(&arguments)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    write(
        fixture.path(),
        "scripts/cordis-walk.ts",
        "moved exact edit site\n",
    );
    let result = Command::new(binary)
        .arg("--root")
        .arg(fixture.path())
        .arg("--apply")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("nothing was written"));
    assert_eq!(
        fs::read_to_string(fixture.path().join("scripts/cordis-walk.ts")).unwrap(),
        "moved exact edit site\n"
    );
}
