//! Full-workspace catalog differential using a read-only pinned compiler oracle.

use std::{error::Error, path::Path, process::Command};

use seekdeep_typert_generator::{
    catalog::{
        CordisCatalogModel, CordisCatalogPolicy, CordisCatalogProjector, render_inherited_page,
        render_page_region,
    },
    model::{FaceModel, SourceDeclarationModel},
};
use serde_json::Value;

const ORACLE: &str = r"
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const { createRequire } = require('node:module');
const root = resolve(process.argv[1]);
const sourceRequire = createRequire(resolve(root, 'package.json'));
sourceRequire('tsx/cjs');
const ts = sourceRequire('typescript');
const source = sourceRequire('./packages/typert/generator/src/cordis-catalog.ts');
const policyPath = resolve(root, 'scripts/gen-cordis-catalog.ts');
const policyText = readFileSync(policyPath, 'utf8');
const policyFile = ts.createSourceFile(policyPath, policyText, ts.ScriptTarget.Latest, true);
const names = new Set(['SERVICE_PAGE', 'EVENT_SCOPE_PAGE', 'LINK_MAP', 'FOUNDATION_TYPE_NAMES', 'TYPE_LINK_EXEMPTIONS', 'CORDIS_CATALOG_POLICY']);
const declarations = policyFile.statements.filter(statement => ts.isVariableStatement(statement)
  && statement.declarationList.declarations.some(declaration => ts.isIdentifier(declaration.name) && names.has(declaration.name.text)))
  .map(statement => statement.getText(policyFile).replace(/^export\s+/u, '')).join('\n');
const policyData = new Function(ts.transpileModule(declarations + '\nreturn {SERVICE_PAGE,EVENT_SCOPE_PAGE,CORDIS_CATALOG_POLICY};', {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None },
}).outputText)();
const policy = policyData.CORDIS_CATALOG_POLICY;
const { projector, model } = source.projectCordisCatalog(root, policy);
const pages = [...new Set([...Object.values(policyData.SERVICE_PAGE), ...Object.values(policyData.EVENT_SCOPE_PAGE)])].sort().map(page => {
  const services = model.services.filter(service => policyData.SERVICE_PAGE[service.key] === page);
  const events = model.events.filter(event => policyData.EVENT_SCOPE_PAGE[event.scope] === page);
  const expected = source.renderPageRegion(page, services, events, policy);
  const committed = [page, page.replace(/\.md$/, '.zh.md')].map(side => {
    const text = readFileSync(resolve(root, 'docs/subsystems', side), 'utf8');
    const begin = text.indexOf(source.REGION_BEGIN);
    const end = text.indexOf(source.REGION_END);
    if (begin < 0 || end < begin) throw new Error('missing source region: ' + side);
    return text.slice(begin, end + source.REGION_END.length);
  });
  return { page, services, events, expected, committed };
});
const inherited = source.renderInheritedPage(policy);
const runtimeApi = projector.renderRuntimeApi(model);
process.stdout.write(JSON.stringify({
  face: projector.face, sourceDeclarations: projector.sourceDeclarations, policy, model, pages,
  inherited, inheritedCommitted: readFileSync(resolve(root, 'docs/cordis-api/inherited.md'), 'utf8'),
  runtimeApi, runtimeCommitted: readFileSync(resolve(root, 'packages/extensions/tool-cordis/src/api-catalog.ts'), 'utf8'),
}, (key, value) => value instanceof Set ? [...value] : typeof value === 'bigint' ? { $bigint: String(value) } : value));
";

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let first = arguments
        .first()
        .ok_or("usage: catalog_parity <source-root> [capture-file] | --input <capture-file>")?;
    let payload = if first == "--input" {
        std::fs::read(arguments.get(1).ok_or("--input needs a capture file")?)?
    } else {
        let source = Path::new(first);
        verify_pin(source)?;
        let output = Command::new("node")
            .args(["-e", ORACLE])
            .arg(source)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        if let Some(path) = arguments.get(1) {
            std::fs::write(path, &output.stdout)?;
        }
        output.stdout
    };
    verify(&serde_json::from_slice(&payload)?)
}

fn verify_pin(source: &Path) -> Result<(), Box<dyn Error>> {
    let head = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let pinned = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("missing source pin")?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != pinned {
        return Err("oracle differs from SOURCE_SNAPSHOT".into());
    }
    Ok(())
}

fn verify(payload: &Value) -> Result<(), Box<dyn Error>> {
    let face: FaceModel = serde_json::from_value(payload["face"].clone())?;
    let declarations: Vec<SourceDeclarationModel> =
        serde_json::from_value(payload["sourceDeclarations"].clone())?;
    let policy: CordisCatalogPolicy = serde_json::from_value(payload["policy"].clone())?;
    let projector = CordisCatalogProjector::new(&face, &declarations, &policy);
    let model = projector.project()?;
    same(
        "projected model",
        &serde_json::to_value(&model)?,
        &payload["model"],
    )?;
    let inherited = serde_json::to_value(render_inherited_page(&policy))?;
    same("inherited page", &inherited, &payload["inherited"])?;
    same(
        "committed inherited page",
        &inherited,
        &payload["inheritedCommitted"],
    )?;
    let runtime = serde_json::to_value(projector.render_runtime_api(&model)?)?;
    for field in ["runtimeApi", "runtimeCommitted"] {
        let expected = payload[field]
            .as_str()
            .ok_or("missing runtime text")?
            .replace(
                "@deepseek-ai/dsh-tool-cordis",
                "@seekdeep-ai/seekdeep-tool-cordis",
            );
        same(field, &runtime, &Value::String(expected))?;
    }
    let pages = payload["pages"].as_array().ok_or("missing source pages")?;
    for page in pages {
        let name = page["page"].as_str().ok_or("missing page name")?;
        let selected: CordisCatalogModel = serde_json::from_value(
            serde_json::json!({"services":page["services"],"events":page["events"]}),
        )?;
        let actual = Value::String(render_page_region(
            name,
            &selected.services,
            &selected.events,
            &policy,
        )?);
        same(name, &actual, &page["expected"])?;
        for committed in page["committed"]
            .as_array()
            .ok_or("missing committed regions")?
        {
            same(name, &actual, committed)?;
        }
    }
    let data = projector.runtime_catalog(&model)?;
    let data_text = serde_json::to_string(&data)?
        .replace("DSH Node.js process", "SeekDeep Harness Host process")
        .replace("DSH process", "SeekDeep Harness process")
        .replace("DSH objects", "SeekDeep Harness objects")
        .replace("@deepseek-ai/dsh-", "@seekdeep-ai/seekdeep-")
        .replace("dsh-", "seekdeep-")
        .replace("DshEnvironment", "SeekdeepEnvironment")
        .replace("DSH_", "SEEKDEEP_");
    same(
        "native runtime catalog data",
        &serde_json::from_str(&data_text)?,
        &serde_json::from_str(include_str!("../../tool-cordis/data/api-catalog.json"))?,
    )?;
    println!(
        "{} services, {} events, {} runtime types, {} bilingual page regions: exact source parity",
        model.services.len(),
        model.events.len(),
        data.types.len(),
        pages.len()
    );
    Ok(())
}

fn same(path: &str, actual: &Value, expected: &Value) -> Result<(), Box<dyn Error>> {
    if actual == expected {
        return Ok(());
    }
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            for (key, actual) in actual {
                same(
                    &format!("{path}.{key}"),
                    actual,
                    expected.get(key).unwrap_or(&Value::Null),
                )?;
            }
        }
        (Value::Array(actual), Value::Array(expected)) => {
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                same(&format!("{path}[{index}]"), actual, expected)?;
            }
        }
        (Value::String(actual), Value::String(expected)) => {
            for (index, (actual, expected)) in actual.lines().zip(expected.lines()).enumerate() {
                if actual != expected {
                    return Err(format!(
                        "{path} line {}: actual {:?}, expected {:?}",
                        index + 1,
                        actual.chars().take(240).collect::<String>(),
                        expected.chars().take(240).collect::<String>()
                    )
                    .into());
                }
            }
        }
        _ => {}
    }
    Err(format!("{path}: values or collection sizes differ").into())
}
