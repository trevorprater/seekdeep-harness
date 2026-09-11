//! Semantic event graph parity against the pinned TypeScript collector.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use seekdeep_typert_generator::analyzer::{
    repository::TypeScriptProject,
    repository_graphs::{EventRelations, collect_event_relations, collect_package_sources},
    run_with_stack,
};
use serde_json::json;
use tempfile::TempDir;

const SOURCE: &str = "/Users/trevor/ws/deepseek-harness";
const LIBRARY: &str = "/Users/trevor/ws/deepseek-harness/node_modules/typescript/lib/typescript.js";

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn oracle(root: &Path, packages: Option<&[&str]>) -> EventRelations {
    let script = r"
const fs = require('node:fs');
const vm = require('node:vm');
const ts = require(process.argv[1]+'/node_modules/typescript/lib/typescript.js');
const {pathToFileURL} = require('node:url');
const input=JSON.parse(fs.readFileSync(0,'utf8'));
(async()=>{
 const {TypeScriptProject}=await import(pathToFileURL(process.argv[1]+'/scripts/ts-project.ts'));
 const filename=process.argv[1]+'/scripts/gen-doc-graphs.ts';
 const raw=fs.readFileSync(filename,'utf8');
 const source=ts.createSourceFile(filename,raw,ts.ScriptTarget.Latest,true);
 const names=new Set(['EventRelationCollector','isDirectCallee','unwrapExpression','finiteStringTypeValues','isConstDeclaration','hasExportModifier','addAll','unionSets','collectPackageSources','EVENT_API_METHODS']);
 const selected=source.statements.filter(s=>(ts.isClassDeclaration(s)||ts.isFunctionDeclaration(s))?names.has(s.name?.text):ts.isVariableStatement(s)&&s.declarationList.declarations.some(d=>names.has(d.name.getText(source))));
 const rawCode=selected.map(s=>s.getText(source).replace(/^export /,'')).join('\n')+'\nconst project=new TypeScriptProject(input.root); const sources=collectPackageSources(project).filter(s=>!input.packages||input.packages.includes(s.pkg)); const result=new EventRelationCollector(project,sources).collect(); JSON.stringify(Object.fromEntries([...result].map(([event,relation])=>[event,{dispatchers:Object.fromEntries([...relation.dispatchers].map(([pkg,methods])=>[pkg,[...methods]])),listeners:[...relation.listeners]}])))';
 const code=ts.transpileModule(rawCode,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
 process.stdout.write(vm.runInNewContext(code,{ts,TypeScriptProject,input}));
})().catch(error=>{console.error(error);process.exitCode=1});
";
    let mut child = Command::new("node")
        .args(["--experimental-transform-types", "-e", script, SOURCE])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::to_string(&json!({"root":root,"packages":packages}))
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fixture() -> TempDir {
    let root = TempDir::new().unwrap();
    write(root.path(), "tsconfig.host.json", &json!({"compilerOptions":{"target":"es2022","module":"esnext","moduleResolution":"bundler","allowImportingTsExtensions":true,"noEmit":true,"skipLibCheck":true,"types":[]},"include":["vendor/**/*.ts","packages/**/*.ts"]}).to_string());
    write(
        root.path(),
        "vendor/cordis/src/context.ts",
        "export class Context { private brand!: void; emit(...args:unknown[]):void{}; on(...args:unknown[]):void{}; once(...args:unknown[]):void{}; parallel(...args:unknown[]):void{}; serial(...args:unknown[]):void{}; waterfall(...args:unknown[]):void{} }\n",
    );
    write(
        root.path(),
        "vendor/cordis/src/events.ts",
        "export class EventsService { dispatch(type:string,args:unknown[]):unknown[]{return [type,args]} }\n",
    );
    write(
        root.path(),
        "packages/core/agent/src/dispatch.ts",
        "export interface AgentEventDispatch { emit(name:'forward/a'|'forward/b', ...args:unknown[]):void }\nexport function emitAgentEvent(_ctx:unknown,_agent:unknown,_event:string):void{}\n",
    );
    write(
        root.path(),
        "packages/fix/pkga/src/index.ts",
        "import { EventsService } from '../../../../vendor/cordis/src/events.ts'\ndeclare const events:EventsService\nfunction fireLocal(args:[string]):void { void events.dispatch('emit',args) }\nfireLocal(['pkga/local-event'])\nfunction fireAliased(args:[string]):void { void events.dispatch('emit',args) }\nexport const aliased=fireAliased\n",
    );
    write(
        root.path(),
        "packages/fix/pkgb/src/index.ts",
        "import { aliased } from '../../pkga/src/index.ts'\naliased(['pkgb/aliased-event'])\n",
    );
    write(
        root.path(),
        "packages/fix/pkgc/src/globals.ts",
        "declare var gEvents:import('../../../../vendor/cordis/src/events.ts').EventsService\n",
    );
    write(
        root.path(),
        "packages/fix/pkgc/src/helper.ts",
        "function scriptFire(args:[string]):void { void gEvents.dispatch('emit',args) }\n",
    );
    write(
        root.path(),
        "packages/fix/pkgc/src/caller.ts",
        "scriptFire(['pkgc/script-event'])\n",
    );
    write(
        root.path(),
        "packages/fix/semantic/src/index.ts",
        r"
import { Context } from '../../../../vendor/cordis/src/context.ts'
import { EventsService } from '../../../../vendor/cordis/src/events.ts'
import { emitAgentEvent as contained, type AgentEventDispatch } from '../../../core/agent/src/dispatch.ts'
declare const ctx:Context
declare const events:EventsService
declare const dispatch:AgentEventDispatch
declare const bad:any
declare const mystery:unknown
declare const impossible:never
declare const receiver:object
ctx.emit('direct/event')
ctx.on('direct/event',()=>{})
ctx.once('listen/only',()=>{})
ctx.serial(receiver,'second/argument')
ctx.waterfall('waterfall/event')
ctx.parallel('template/event')
bad.emit('fake/any')
mystery.emit('fake/unknown')
impossible.emit('fake/never')
({emit(...args:unknown[]) {}}).emit('fake/structural')
declare const finite:'union/a'|'union/b'
declare const widened:string
ctx.emit(finite)
ctx.emit(widened)
function generic<T extends 'generic/a'|'generic/b'>(event:T) {ctx.emit(event)}
dispatch.emit('forward/a')
const forwarded:AgentEventDispatch={emit(name) {ctx.emit(name)}}
contained(ctx,{},'contained/event')
function emitAgentEvent(_a:unknown,_b:unknown,_c:string) {}
emitAgentEvent(ctx,{},'fake/emitter-name')
const args = [receiver,'array/second'] as const
events.dispatch('emit',args)
const condition = true as boolean
events.dispatch('emit',condition ? ['conditional/a'] : ['conditional/b'])
let mutable = ['mutable/ignored']
events.dispatch('emit',mutable)
events.dispatch('emit',[...args,'spread/second'])
function nested(args:unknown[]) { events.dispatch('emit', args) }
function outer(args:unknown[]) { nested(args) }
outer(['nested/recovery'])
export function publicHelper(args:unknown[]) { events.dispatch('emit', args) }
publicHelper(['exported/ignored'])
function wrapped(args:unknown[]) { events.dispatch('emit',args) }
(wrapped satisfies typeof wrapped)(['wrapper/satisfies'])
(wrapped as typeof wrapped)(['wrapper/as'])
wrapped!(['wrapper/non-null'])
function recursive(args:unknown[]) { recursive(args); events.dispatch('emit',args) }
recursive(['recursive/bounded'])
",
    );
    root
}

fn native(root: &Path, packages: Option<&[&str]>) -> EventRelations {
    let mut project = TypeScriptProject::with_compiler(root, Path::new(LIBRARY)).unwrap();
    let mut sources = collect_package_sources(&mut project).unwrap();
    if let Some(packages) = packages {
        sources.retain(|source| packages.contains(&source.pkg.as_str()));
    }
    collect_event_relations(&mut project, &sources).unwrap()
}

#[test]
fn source_local_alias_escape_global_script_and_semantic_receiver_cases_match() {
    run_with_stack(|| {
        let root = fixture();
        for packages in [
            Some(vec!["pkga", "pkgb"]),
            Some(vec!["pkgc"]),
            Some(vec!["semantic"]),
            None,
        ] {
            let refs = packages.as_deref();
            let actual = native(root.path(), refs);
            assert_eq!(
                actual,
                oracle(root.path(), refs),
                "package selection {packages:?}"
            );
            if refs == Some(&["semantic"][..]) {
                for key in [
                    "direct/event",
                    "second/argument",
                    "union/a",
                    "union/b",
                    "generic/a",
                    "generic/b",
                    "contained/event",
                    "array/second",
                    "conditional/a",
                    "conditional/b",
                    "nested/recovery",
                    "wrapper/satisfies",
                    "recursive/bounded",
                ] {
                    assert!(actual.contains_key(key), "missing {key}");
                }
                for key in [
                    "fake/any",
                    "fake/unknown",
                    "fake/never",
                    "fake/structural",
                    "fake/emitter-name",
                    "mutable/ignored",
                    "exported/ignored",
                    "forward/b",
                ] {
                    assert!(!actual.contains_key(key), "unexpected {key}");
                }
            }
        }
    });
}

#[test]
fn source_whole_repository_event_relations_match_byte_for_byte_as_data() {
    run_with_stack(|| {
        let actual = native(Path::new(SOURCE), None);
        let expected = oracle(Path::new(SOURCE), None);
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(actual.len(), 60, "pinned source event relation count");
    });
}

#[test]
fn missing_program_type_is_a_failure() {
    run_with_stack(|| {
        let root = fixture();
        std::fs::write(
            root.path().join("vendor/cordis/src/context.ts"),
            "export class Renamed {}\n",
        )
        .unwrap();
        let mut project =
            TypeScriptProject::with_compiler(root.path(), Path::new(LIBRARY)).unwrap();
        let sources = collect_package_sources(&mut project).unwrap();
        assert!(
            collect_event_relations(&mut project, &sources)
                .unwrap_err()
                .to_string()
                .contains("cannot resolve TypeScript type Context")
        );
    });
}
