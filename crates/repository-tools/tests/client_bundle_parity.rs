//! Differential coverage of the Client build preset against the pinned source.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use seekdeep_repository_tools::client_bundle::{self, ClientBundleOptions, ClientModuleId};
use serde_json::{Value, json};

fn source_root() -> PathBuf {
    std::env::var_os("SEEKDEEP_PARITY_SOURCE").map_or_else(
        || PathBuf::from("/Users/trevor/ws/deepseek-harness"),
        PathBuf::from,
    )
}

fn source_results(requests: &[Value]) -> Vec<Value> {
    let source = source_root();
    let script = r"
import { pathToFileURL } from 'node:url';
const api = await import(pathToFileURL(process.argv[1] + '/packages/client/tsdown.client.ts').href);
const requests = JSON.parse(await new Promise(resolve => { let input = ''; process.stdin.setEncoding('utf8'); process.stdin.on('data', value => input += value); process.stdin.on('end', () => resolve(input)); }));
const output = [];
for (const request of requests) {
  try {
    if (request.nodeEnv === undefined) delete process.env.NODE_ENV; else process.env.NODE_ENV = request.nodeEnv;
    const env = Object.hasOwn(request, 'face') ? { DSH_BUILD_FACE: request.face } : {};
    const preset = () => api.clientBundle(request.id ?? '@deepseek-ai/dsh-client-test', request.entries ?? ['lib/types/index.js'], request.options ?? {})({ env });
    let value;
    if (request.operation === 'preset') value = preset();
    else if (request.operation === 'library') value = api.clientLibrary(request.id, request.entries)({ env });
    else if (request.operation === 'only') value = api.clientOnly(request.configs)({ env });
    else {
      const config = preset().find(config => config.platform === 'browser');
      if (request.operation === 'import') {
        config.plugins.find(plugin => plugin.name === 'dsh-client-bundle-purity').resolveId(request.specifier);
        value = { external: config.noExternal(request.specifier) === undefined };
      } else if (request.operation === 'sourceMap') value = config.outputOptions.sourcemapPathTransform(request.source, request.sourcemap);
      else if (request.operation === 'css') {
        const plugin = config.plugins.find(plugin => plugin.name === 'dsh-css-modules-inline');
        const virtualId = plugin.resolveId(request.file, request.importer);
        const watched = [];
        const code = await plugin.load.call({ addWatchFile: file => watched.push(file) }, virtualId);
        const classes = (await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'))).default;
        const css = JSON.parse(code.match(/^const css = (.*);$/m)[1]);
        value = { css, classes, watched, virtualId };
      }
    }
    output.push({ value });
  } catch (error) { output.push({ error: error.message }); }
}
process.stdout.write(JSON.stringify(output));
";
    let mut child = Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(script)
        .arg(&source)
        .current_dir(&source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(requests).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn normalize(value: &Value) -> Value {
    serde_json::from_str(
        &value
            .to_string()
            .replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
            .replace("@deepseek-ai/", "@seekdeep-ai/")
            .replace("DSH_BUILD_FACE", "SEEKDEEP_BUILD_FACE")
            .replace("dsh-client-bundle-purity", "seekdeep-client-bundle-purity")
            .replace("dsh-css-modules-inline", "seekdeep-css-modules-inline"),
    )
    .unwrap()
}

#[test]
fn every_build_face_preserves_overrides_companions_defines_and_library_outputs() {
    let mut requests = Vec::new();
    for face in [None, Some(json!("host")), Some(json!("client"))] {
        for host_phase in [false, true] {
            for node_env in [None, Some("development"), Some("")] {
                let mut request = json!({ "operation":"preset", "id":"@deepseek-ai/dsh-client-test", "entries":["lib/types/index.js","lib/types/invariant.js"], "options":{"hostPhase":host_phase,"companions":[{"entry":["companion.js"],"name":"companion"}],"lib":{"target":"es2022","custom":true}} });
                if let Some(face) = &face {
                    request["face"] = face.clone();
                }
                if let Some(node_env) = node_env {
                    request["nodeEnv"] = json!(node_env);
                }
                requests.push(request);
            }
        }
    }
    let expected = source_results(&requests);
    for (request, expected) in requests.iter().zip(expected) {
        let options: ClientBundleOptions =
            serde_json::from_value(request["options"].clone()).unwrap();
        let result = client_bundle::client_bundle(
            &ClientModuleId("@seekdeep-ai/seekdeep-client-test".to_owned()),
            &[
                "lib/types/index.js".to_owned(),
                "lib/types/invariant.js".to_owned(),
            ],
            &options,
            client_bundle::build_face(request.get("face")).unwrap(),
            request.get("nodeEnv").and_then(Value::as_str),
        );
        assert_eq!(json!({"value":result}), normalize(&expected), "{request}");
    }
}

#[test]
fn invalid_faces_and_exact_import_boundaries_match_the_source() {
    let faces = [
        json!(null),
        json!(true),
        json!(5),
        json!("CLIENT"),
        json!([]),
        json!({}),
    ];
    let requests: Vec<_> = faces
        .iter()
        .map(|face| json!({"operation":"preset","face":face}))
        .collect();
    for (face, expected) in faces.iter().zip(source_results(&requests)) {
        assert_eq!(
            json!({"error":client_bundle::build_face(Some(face)).unwrap_err().to_string()}),
            normalize(&expected)
        );
    }
    let imports = [
        "react",
        "react-dom/client",
        "zod",
        "node:fs",
        "@deepseek-ai/cordis",
        "@deepseek-ai/dsh-client-web-react",
        "@deepseek-ai/dsh-client-web-react/store",
        "@deepseek-ai/dsh-client-runtime/client",
        "@deepseek-ai/dsh-client-runtime",
        "@deepseek-ai/dsh-host-apiproxy/api",
        "@deepseek-ai/dsh-session/surface",
        "@deepseek-ai/dsh-brand",
        "@deepseek-ai/dsh-branding",
        "@deepseek-ai/dsh-goal/remote",
        "@deepseek-ai/dsh-goal/remote/nested",
        "@deepseek-ai/dsh-/remote",
        "@deepseek-ai/dsh-a--b/remote",
        "@deepseek-ai/dsh-A/remote",
        "@deepseek-ai/dsh-a-2/remote",
        "@deepseek-ai/cosmokit",
        "@deepseek-ai/cosmokit/a",
        "@deepseek-ai/schemastery/a",
        "@deepseek-ai/cosmokit-other",
        "@deepseek-ai/dsh-agent",
        "@deepseek-ai/dsh-client-ui-layout/client",
    ];
    let requests: Vec<_> = imports
        .iter()
        .map(|specifier| json!({"operation":"import","specifier":specifier}))
        .collect();
    for (specifier, expected) in imports.iter().zip(source_results(&requests)) {
        let specifier = normalize(&json!(specifier)).as_str().unwrap().to_owned();
        let actual = match client_bundle::check_import(&specifier) {
            Ok(()) => json!({"value":{"external":client_bundle::is_external(&specifier)}}),
            Err(error) => json!({"error":error.to_string()}),
        };
        assert_eq!(actual, normalize(&expected), "{specifier}");
    }
}

#[test]
fn browser_maps_keep_package_groups_and_leave_external_sources_alone() {
    let root = source_root();
    let sources = [
        (
            "../src/client/GoalBar.tsx",
            "packages/client/ui-goal/lib/client.js.map",
        ),
        (
            "../src/client/index.ts",
            "packages/host/directory-picker-native/lib/client.js.map",
        ),
        (
            "../../../host/apiproxy/src/api/rpc.ts",
            "packages/client/connection/lib/client.js.map",
        ),
        (
            "../../../../node_modules/.pnpm/zod@4.4.3/node_modules/zod/index.js",
            "packages/client/connection/lib/client.js.map",
        ),
        (
            "node_modules/dependency/index.js",
            "packages/client/connection/lib/client.js.map",
        ),
        (
            "../src/client/目录.tsx",
            "packages/client/ui-goal/lib/client.js.map",
        ),
    ];
    let requests: Vec<_> = sources.iter().map(|(source, map)| json!({"operation":"sourceMap","source":source,"sourcemap":root.join(map)})).collect();
    for ((source, map), expected) in sources.iter().zip(source_results(&requests)) {
        assert_eq!(
            json!({"value":client_bundle::browser_source_path(source, &root.join(map), &root)}),
            expected
        );
    }
}

#[test]
fn native_css_matches_source_hashes_minification_and_physical_asset_fallback() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("packages/client/fixture");
    let source_dir = package.join("src/client");
    std::fs::create_dir_all(&source_dir).unwrap();
    let samples = [
        ".root { color: red; margin: 0px 0px 0px 0px; } .other { composes: root; display: block; }\n",
        "@keyframes pulse { from {opacity: 0} to {opacity: 1} } .root { animation: pulse 1s; & > .child { color: #ffffff; } }\n",
        ":global(.external) .root { display: flex; --tone: blue; color:var(--tone); }\n",
    ];
    for (index, sample) in samples.iter().enumerate() {
        let file = source_dir.join(format!("Fixture{index}.module.css"));
        std::fs::write(&file, sample).unwrap();
        let import = format!("./Fixture{index}.module.css");
        let importer = package.join("lib/types/client/index.js");
        assert_eq!(client_bundle::source_asset_path(&import, &importer), file);
        let expected =
            source_results(&[json!({"operation":"css","file":import,"importer":importer})]);
        let result = client_bundle::compile_css_module(
            &ClientModuleId("@seekdeep-ai/seekdeep-client-test".to_owned()),
            &file,
        )
        .unwrap();
        assert_eq!(json!(result.css), expected[0]["value"]["css"]);
        assert_eq!(json!(result.classes), expected[0]["value"]["classes"]);
        assert_eq!(json!([result.watch_file]), expected[0]["value"]["watched"]);
        assert_eq!(
            result.tag_id,
            format!("@seekdeep-ai/seekdeep-client-test/Fixture{index}.module.css")
        );
    }
    let emitted = package.join("lib/types/client/Fixture0.module.css");
    std::fs::create_dir_all(emitted.parent().unwrap()).unwrap();
    std::fs::write(&emitted, ".emitted{}").unwrap();
    assert_eq!(
        client_bundle::source_asset_path(
            "./Fixture0.module.css",
            &package.join("lib/types/client/index.js")
        ),
        emitted
    );
    assert_eq!(
        client_bundle::source_asset_path("./missing.css", Path::new("/fixture/index.js")),
        PathBuf::from("/fixture/missing.css")
    );
}
