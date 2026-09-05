//! Programmable generated-Client fake for Rust object-layer tests.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use indexmap::IndexMap;
use seekdeep_client_runtime::{ClientRpcError, ClientRpcResult};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FakeCall {
    pub(crate) method: String,
    pub(crate) payload: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StreamItem {
    Frame(Value),
    End,
    Failure(String),
}

#[derive(Default)]
pub(crate) struct FakeStreamHub {
    readers: RefCell<Vec<Rc<RefCell<VecDeque<StreamItem>>>>>,
    pub(crate) suppress_open: RefCell<bool>,
    pub(crate) hold_open: RefCell<bool>,
    held_opens: RefCell<usize>,
    fired_opens: RefCell<usize>,
}

impl FakeStreamHub {
    pub(crate) fn open(&self, requests_open: bool) -> Rc<RefCell<VecDeque<StreamItem>>> {
        let reader = Rc::new(RefCell::new(VecDeque::new()));
        self.readers.borrow_mut().push(reader.clone());
        if requests_open && !*self.suppress_open.borrow() {
            if *self.hold_open.borrow() {
                *self.held_opens.borrow_mut() += 1;
            } else {
                *self.fired_opens.borrow_mut() += 1;
            }
        }
        reader
    }

    pub(crate) fn release_opens(&self) {
        let held = std::mem::take(&mut *self.held_opens.borrow_mut());
        *self.fired_opens.borrow_mut() += held;
    }

    pub(crate) fn fired_opens(&self) -> usize {
        *self.fired_opens.borrow()
    }

    pub(crate) fn connection_count(&self) -> usize {
        self.readers.borrow().len()
    }

    pub(crate) fn push(&self, frame: &Value) {
        for reader in self.readers.borrow().iter() {
            reader
                .borrow_mut()
                .push_back(StreamItem::Frame(frame.clone()));
        }
    }

    pub(crate) fn end(&self) {
        for reader in self.readers.borrow().iter() {
            reader.borrow_mut().push_back(StreamItem::End);
        }
    }

    pub(crate) fn fail(&self, message: &str) {
        for reader in self.readers.borrow().iter() {
            reader
                .borrow_mut()
                .push_back(StreamItem::Failure(message.to_owned()));
        }
    }
}

#[derive(Default)]
pub(crate) struct FakeApiClient {
    calls: RefCell<Vec<FakeCall>>,
    scripted: RefCell<ScriptedResults>,
    pub(crate) last_search_signal: RefCell<Option<String>>,
    pub(crate) mux: FakeStreamHub,
    pub(crate) host: FakeStreamHub,
}

impl FakeApiClient {
    #[allow(clippy::unused_async)]
    pub(crate) async fn call(
        &self,
        method: &str,
        payload: Value,
    ) -> Result<ClientRpcResult<Value>, String> {
        self.calls.borrow_mut().push(FakeCall {
            method: method.to_owned(),
            payload: payload.clone(),
        });
        self.scripted
            .borrow_mut()
            .get_mut(method)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| Ok(default_result(method, &payload)))
    }

    pub(crate) async fn search(
        &self,
        payload: Value,
        signal: Option<&str>,
    ) -> Result<ClientRpcResult<Value>, String> {
        *self.last_search_signal.borrow_mut() = signal.map(ToOwned::to_owned);
        self.call("session.search", payload).await
    }

    pub(crate) fn script(&self, method: &str, result: Result<ClientRpcResult<Value>, String>) {
        self.scripted
            .borrow_mut()
            .entry(method.to_owned())
            .or_default()
            .push_back(result);
    }

    pub(crate) fn calls_of(&self, method: &str) -> Vec<Value> {
        self.calls
            .borrow()
            .iter()
            .filter(|call| call.method == method)
            .map(|call| call.payload.clone())
            .collect()
    }
}

pub(crate) const METHOD_ROSTER: &[&str] = &[
    "session.list",
    "session.search",
    "session.create",
    "session.history",
    "session.models",
    "session.selectModel",
    "session.rename",
    "session.fork",
    "session.prompt",
    "session.attachment",
    "session.updateQueue",
    "session.cancel",
    "subagent.list",
    "subagent.history",
    "subagent.prompt",
    "subagent.interrupt",
    "host.describe",
    "host.pickDirectory",
    "host.listDirectory",
    "host.createDirectory",
    "host.openPath",
    "workspace.list",
    "workspace.create",
    "workspace.rename",
    "workspace.delete",
    "workspace.insertBefore",
    "workspace.insertSessionBefore",
    "workspace.archiveSession",
    "agentPreset.list",
    "agentPreset.select",
    "agentPreset.read",
    "agentPreset.copy",
    "agentPreset.openDocument",
    "agentPreset.remove",
    "skill.list",
    "goal.create",
    "goal.edit",
    "goal.pause",
    "goal.resume",
    "goal.complete",
    "goal.clear",
    "settings.describe",
    "settings.openDocument",
    "settings.update",
    "settings.replace",
    "settings.mutate",
    "credentials.describe",
    "credentials.set",
    "credentials.unset",
    "llm.providers",
    "llm.models",
    "llm.discoverModels",
    "respond",
];

type ScriptedResult = Result<ClientRpcResult<Value>, String>;
type ScriptedResults = IndexMap<String, VecDeque<ScriptedResult>>;

#[allow(clippy::too_many_lines)]
fn default_result(method: &str, payload: &Value) -> ClientRpcResult<Value> {
    let value = match method {
        "session.list" => json!({"items":[]}),
        "session.search" => json!({"items":[],"hasMore":false}),
        "session.create" => json!({"sessionId":"fk-new"}),
        "session.history" | "subagent.history" => json!({"events":[],"hasMore":false}),
        "session.models" => json!({
            "current":{"provider":"deepseek-official","model":"deepseek-v4-flash"},
            "routable":true,
            "groups":[{"id":"deepseek-official","name":"DeepSeek","models":[{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash"}]}],
            "failures":[],
        }),
        "session.selectModel" => json!({
            "selected":{"provider":payload["provider"].clone(),"model":payload["model"].clone()}
        }),
        "session.rename" => json!({"title":"fk-renamed","seq":0}),
        "session.fork" => json!({"sessionId":"fk-fork"}),
        "session.prompt" | "session.updateQueue" | "session.cancel" | "subagent.interrupt" => {
            json!({"accepted":true})
        }
        "session.attachment" => json!({
            "attachment":{"attachmentId":"a","mediaType":"image/png","bytes":1,"width":1,"height":1},
            "data":"AA==",
        }),
        "subagent.list" => json!({"entries":[],"parentAvailable":true}),
        "subagent.prompt" => json!({"messageId":"fake-message"}),
        "host.describe" => json!({
            "version":"0-fake","cwd":"/f","attachedSessions":0,"canOpenPath":true,
        }),
        "host.pickDirectory" => json!({"path":null}),
        "host.listDirectory" => json!({
            "path":"/home/fake","home":"/home/fake",
            "crumbs":[{"name":"/","path":"/","hidden":false}],"entries":[],"truncated":false,
        }),
        "host.createDirectory" => json!({"path":"/home/fake/new"}),
        "host.openPath" | "agentPreset.openDocument" | "settings.openDocument" => {
            json!({"opened":true})
        }
        "workspace.list" => json!({"items":[],"archivedSessionIds":[]}),
        "workspace.create" => json!({"workspace":fake_workspace("fk-ws"),"created":true}),
        "workspace.rename" | "workspace.insertSessionBefore" => {
            json!({"workspace":fake_workspace("fk-ws")})
        }
        "workspace.delete" => json!({"deleted":true}),
        "workspace.insertBefore" => json!({"workspaceIds":[]}),
        "workspace.archiveSession" => json!({"archivedSessionIds":[payload["sessionId"].clone()]}),
        "agentPreset.list" => json!({"presets":[],"authorable":false,"hasDocument":false}),
        "agentPreset.select" | "agentPreset.copy" => {
            json!({"agentPreset":payload["agentPreset"].clone()})
        }
        "agentPreset.read" => json!({
            "agentPreset":payload["agentPreset"].clone(),"trust":"user","content":"",
        }),
        "agentPreset.remove" | "credentials.set" | "credentials.unset" => json!({}),
        "skill.list" => json!({"skills":[]}),
        "goal.create" | "goal.edit" | "goal.pause" | "goal.resume" | "goal.complete" => {
            json!({"ref":{"id":"fake-goal","revision":1}})
        }
        "goal.clear" => json!({"cleared":true}),
        "settings.describe" => json!({"writable":true,"hasDocument":false,"namespaces":[]}),
        "settings.update" | "settings.replace" | "settings.mutate" => json!({
            "ns":"fake","schema":{},"value":{},"applies":"live","secrets":[],"revision":0,
        }),
        "credentials.describe" => json!({"credentials":{}}),
        "llm.providers" => json!({"providers":[]}),
        "llm.models" => json!({"groups":[],"failures":[]}),
        "llm.discoverModels" => json!({"models":[]}),
        "respond" => json!({"accepted":true}),
        _ => {
            return ClientRpcResult::Failure(ClientRpcError {
                code: "internal".to_owned(),
                message: format!("FakeApiClient has no default for {method}"),
                details: serde_json::Map::new(),
            });
        }
    };
    ClientRpcResult::Success(Some(value))
}

fn fake_workspace(id: &str) -> Value {
    json!({
        "workspaceId":id,"path":format!("/w/{id}"),"title":id,"sessionIds":[],
        "createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z",
    })
}
