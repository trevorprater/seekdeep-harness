//! Compare model decisions and complete snapshot rendering with the pinned Python oracle.

use std::{path::Path, process::Command};

use seekdeep_python_runtime_smoke::{model::completion_chunks, snapshot};
use serde_json::Value;

const ORACLE: &str = r"
import copy, json, pathlib, runpy, sys, types
source = runpy.run_path(str(pathlib.Path(sys.argv[1]) / 'scripts/smoke-python-runtime.py'))
cases = []
def tools(names):
    return [{'type':'function','function':{'name':name}} for name in names]
def request(prompt, names=(), system=None):
    messages = [] if system is None else [{'role':'system','content':system}]
    messages.append({'role':'user','content':prompt})
    return {'messages':messages,'tools':tools(names)}
def followup(id, name, text, names=(), extra=()):
    return {'messages':[*extra, {'role':'assistant','tool_calls':[{'id':id,'function':{'name':name}}]}, {'role':'tool','content':text}], 'tools':tools(names)}
cases.extend([{}, {'messages':[]}, {'messages':[None]}, {'messages':[7]}, request('hello'), request([{'text':'hello'},None,{'text':4}])])
for key, names in [('CODE_PROMPT',['run_code']), ('WORKFLOW_PROMPT',['workflow']), ('SNAPSHOT_PROMPT',['cordis_define']), ('SNAPSHOT_DIRECT_CHILD_PROMPT',[]), ('SNAPSHOT_WORKFLOW_CHILD_PROMPT',[])]:
    body = request(source[key], names)
    cases.append(body)
    delayed = copy.deepcopy(body)
    delayed['messages'].append({'role':'user','content':'runtime context follows the scenario prompt'})
    cases.append(delayed)
    if names:
        missing = copy.deepcopy(body)
        missing.pop('tools')
        cases.append(missing)
        cases.append(request(source[key]))
for name in ['run_code','workflow','unknown']:
    cases.append(followup('call-worker',name,'42'))
    cases.append(followup('call-worker',name,'wrong'))
cases.append({'messages':[{'role':'tool','content':'42'}]})
minimal = source['MINIMAL_PROMPT'] + '\n' + source['MINIMAL_EDITOR_PATH_PREFIX'] + '/tmp/editor.txt'
minimal_request = request(minimal, ['bash','str_replace_editor'], source['MINIMAL_SYSTEM_PROMPT'])
cases.append(minimal_request)
cases.append(request(minimal, ['bash'], source['MINIMAL_SYSTEM_PROMPT']))
cases.append(request(minimal, ['bash','str_replace_editor'], 'wrong prompt'))
for path in ['/tmp/editor.txt','/tmp/\u007f\U0001f980.txt','\u001c/tmp/\u00e9.txt\u001f']:
    prompt = source['MINIMAL_PROMPT'] + '\n' + source['MINIMAL_EDITOR_PATH_PREFIX'] + path
    cases.append(followup('minimal-bash-2','bash','COUNT=2 CWD=/tmp', extra=[{'role':'user','content':prompt}]))
for id, name, text in [('minimal-bash-1','bash','COUNT=1'), ('minimal-editor','str_replace_editor','New file created successfully'), ('minimal-bash-2','bash','COUNT=2 CWD=/tmp')]:
    cases.append(followup(id,name,text))
    cases.append(followup(id,name,'wrong'))
    cases.append(followup(id,'wrong',text))
for id, name, text, names in [
    ('advanced-define','cordis_define','Defined snap-1/pkg-1 (Snapshot Double)',['cordis_run']),
    ('advanced-run','cordis_run','snap-1/pkg-1 is running (run-1)',['run_code','snapshot_double']),
    ('advanced-code','run_code','42',['subagent']),
    ('advanced-direct-child','subagent','DIRECT_CHILD_OK',['workflow']),
    ('advanced-workflow','workflow','WORKFLOW_CHILD_OK',['cordis_undefine']),
    ('advanced-undefine','cordis_undefine','Removed dynamic Plugin snap-1 and all of its Packages.',[]),
]:
    cases.append(followup(id,name,text,names))
    cases.append(followup(id,name,'wrong',names))
    cases.append(followup(id,'wrong',text,names))
    if names:
        cases.append(followup(id,name,text,[]))
cases.append(followup('advanced-define','cordis_define','Defined snap-1/pkg-1 (Snapshot Double)',['cordis_run','snapshot_double']))
cases.append(followup('advanced-undefine','cordis_undefine','Removed dynamic Plugin snap-1 and all of its Packages.',['snapshot_double']))
models = []
for body in cases:
    try:
        models.append({'body':body,'expected':source['completion_chunks'](body)})
    except AssertionError as error:
        models.append({'body':body,'error':str(error).split(':',1)[0]})
replacements = [('/tmp/\u00e9/root','{{cwd}}'),('child-long','{{child}}'),('child','{{short}}')]
values = [None, True, 17, '/tmp/\u00e9/root/child-long',
    {'type':'session','createdAt':123,'cwd':'/tmp/\u00e9/root'},
    {'seq':4,'time':123,'role':'assistant','id':'original','nested':{'role':'tool','id':'keep'}},
    {'type':'request/header','data':{'header':{'system':['bulky'],'tools':[{'name':'alpha','schema':{}},None,{'other':'value'},'wrong']}}},
    {'type':'request/header','data':{'header':'unchanged'}},
]
normalized = [{'input':value,'expected':source['normalize_snapshot_value'](value,replacements)} for value in values]
cwd = pathlib.Path('/tmp/\u00e9/\U0001f980')
restore_pairs = [('{{cwd}}',str(cwd)),('{{parent}}',source['SNAPSHOT_SESSION_ID']),('{{workflow-run}}','workflow-run-id'),('{{child-1}}','child-alpha'),('{{child-2}}','child-beta'),('{{agent-1}}','agent-alpha'),('{{agent-2}}','agent-beta')]
def restore(value):
    if isinstance(value,str):
        for token, actual in restore_pairs: value=value.replace(token,actual)
        return value
    if isinstance(value,list): return [restore(item) for item in value]
    if isinstance(value,dict): return {key:restore(item) for key,item in value.items()}
    return value
directory = source['SNAPSHOT_DIRECTORY']
result = restore(json.loads((directory/'result.json').read_text()))
logs = {}
for name in ['session.jsonl','session.1.jsonl','session.2.jsonl']:
    records=[restore(json.loads(line)) for line in (directory/name).read_text().splitlines()]
    logs[records[0]['id']]=records
view = types.SimpleNamespace(**result)
view.notifications=[types.SimpleNamespace(**item) for item in result['notifications']]
files=source['build_snapshot_files'](view,logs,source['snapshot_child_ids'](view),cwd)
print(json.dumps({'models':models,'normalized':normalized,'replacements':replacements,'snapshot':{'result':result,'logs':logs,'cwd':str(cwd),'files':files}},ensure_ascii=True))
";

fn main() -> anyhow::Result<()> {
    let source = std::env::args_os().nth(1).ok_or_else(|| {
        anyhow::anyhow!("usage: python_runtime_smoke_source_parity <pinned-source>")
    })?;
    let source = Path::new(&source).canonicalize()?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .expect("source commit pin");
    let head = Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()?;
    anyhow::ensure!(
        head.status.success() && String::from_utf8_lossy(&head.stdout).trim() == pin,
        "oracle differs from SOURCE_SNAPSHOT"
    );
    let output = Command::new("python3")
        .args(["-c", ORACLE])
        .arg(&source)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "source probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixture: Value = serde_json::from_slice(&output.stdout)?;
    compare_models(&fixture)?;
    let replacements =
        serde_json::from_value::<Vec<(String, String)>>(fixture["replacements"].clone())?;
    for case in fixture["normalized"]
        .as_array()
        .expect("source normalization cases")
    {
        anyhow::ensure!(
            snapshot::normalize_snapshot_value(case["input"].clone(), &replacements)
                == case["expected"],
            "normalization differs: {case}"
        );
    }
    let snapshot = &fixture["snapshot"];
    let files = snapshot::build_snapshot_files(
        &snapshot["result"],
        &snapshot["logs"],
        Path::new(snapshot["cwd"].as_str().expect("source cwd")),
    )?;
    for (name, actual) in files {
        anyhow::ensure!(
            Some(actual.as_str()) == snapshot["files"][&name].as_str(),
            "snapshot rendering differs in {name}"
        );
    }
    println!(
        "{} model cases, {} normalization cases, and all four advanced files match the pinned source",
        fixture["models"].as_array().expect("model cases").len(),
        fixture["normalized"]
            .as_array()
            .expect("normalization cases")
            .len()
    );
    Ok(())
}

fn compare_models(fixture: &Value) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for (index, case) in fixture["models"]
        .as_array()
        .expect("source model cases")
        .iter()
        .enumerate()
    {
        match completion_chunks(&case["body"]) {
            Ok(chunks) => {
                if serde_json::to_value(chunks)? != case["expected"] {
                    failures.push(format!("model case {index} returned different chunks"));
                }
            }
            Err(error) => {
                if !case["error"]
                    .as_str()
                    .is_some_and(|expected| error.to_string().starts_with(expected))
                {
                    failures.push(format!("model case {index}: unexpected error {error}"));
                }
            }
        }
    }
    anyhow::ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}
