//! Default-settings and new-session seat store parity.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use futures::{FutureExt as _, future::LocalBoxFuture};
use seekdeep_client_ui_agent_preset::{
    AgentPresetSeatController, AgentPresetSeatTransport, AgentPresetSectionController,
    AgentPresetSectionStatus, AgentPresetSectionTransport, AgentPresetSettingsController,
    AgentPresetSettingsTransport, AgentPresetStoreStatus, PresetOpenResult, PresetReadValue,
    PresetTrust, RosterPreset, RosterValue, SeatSessionSummary,
};
use seekdeep_identity::SessionId;

#[derive(Default)]
struct Transport {
    roster: RefCell<RosterValue>,
    list_error: RefCell<Option<String>>,
    describe_error: RefCell<Option<String>>,
    update_error: RefCell<Option<String>>,
    select_error: RefCell<Option<String>>,
    writable: RefCell<bool>,
    list_calls: RefCell<u64>,
    updates: RefCell<Vec<String>>,
    selects: RefCell<Vec<(SessionId, String)>>,
}

impl Transport {
    fn new(presets: Vec<RosterPreset>) -> Rc<Self> {
        Rc::new(Self {
            roster: RefCell::new(RosterValue {
                presets,
                authorable: true,
                has_document: true,
            }),
            writable: RefCell::new(true),
            ..Self::default()
        })
    }
}

impl AgentPresetSettingsTransport for Transport {
    fn list(&self) -> LocalBoxFuture<'static, Result<RosterValue, String>> {
        *self.list_calls.borrow_mut() += 1;
        let result = self
            .list_error
            .borrow()
            .clone()
            .map_or_else(|| Ok(self.roster.borrow().clone()), Err);
        async move { result }.boxed_local()
    }

    fn describe_settings(&self) -> LocalBoxFuture<'static, Result<bool, String>> {
        let result = self
            .describe_error
            .borrow()
            .clone()
            .map_or_else(|| Ok(*self.writable.borrow()), Err);
        async move { result }.boxed_local()
    }

    fn update_default(&self, id: String) -> LocalBoxFuture<'static, Result<(), String>> {
        self.updates.borrow_mut().push(id.clone());
        let result = self.update_error.borrow().clone().map_or(Ok(()), Err);
        if result.is_ok() {
            for preset in &mut self.roster.borrow_mut().presets {
                preset.is_default = preset.id == id;
            }
        }
        async move { result }.boxed_local()
    }
}

impl AgentPresetSeatTransport for Transport {
    fn list(&self) -> LocalBoxFuture<'static, Result<RosterValue, String>> {
        AgentPresetSettingsTransport::list(self)
    }

    fn select_session(
        &self,
        session_id: SessionId,
        agent_preset: String,
    ) -> LocalBoxFuture<'static, Result<String, String>> {
        self.selects
            .borrow_mut()
            .push((session_id, agent_preset.clone()));
        let result = self
            .select_error
            .borrow()
            .clone()
            .map_or_else(|| Ok(agent_preset), Err);
        async move { result }.boxed_local()
    }
}

fn preset(id: &str, trust: PresetTrust, is_default: bool) -> RosterPreset {
    RosterPreset {
        id: id.to_owned(),
        trust,
        is_default,
        name: None,
        description: None,
        broken: None,
    }
}

fn roster() -> Vec<RosterPreset> {
    vec![
        preset("standard", PresetTrust::System, true),
        preset("minimal", PresetTrust::System, false),
    ]
}

#[test]
fn settings_store_reads_filters_writes_and_restores_host_truth() {
    let transport = Transport::new(roster());
    let mut broken = preset("damaged", PresetTrust::User, false);
    broken.broken = Some("invalid composition".to_owned());
    transport.roster.borrow_mut().presets.push(broken);
    *transport.writable.borrow_mut() = false;
    let controller = AgentPresetSettingsController::new(transport.clone());
    futures::executor::block_on(controller.load());
    let state = controller.store().snapshot();
    assert_eq!(state.status, AgentPresetStoreStatus::Ready);
    assert!(!state.writable);
    assert_eq!(state.current_value, "standard");
    assert_eq!(
        state
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>(),
        ["standard", "minimal"]
    );

    *transport.writable.borrow_mut() = true;
    futures::executor::block_on(controller.select("minimal"));
    assert_eq!(transport.updates.borrow().as_slice(), ["minimal"]);
    assert_eq!(controller.store().snapshot().current_value, "minimal");

    *transport.update_error.borrow_mut() = Some("read-only settings".to_owned());
    futures::executor::block_on(controller.select("standard"));
    let failed = controller.store().snapshot();
    assert_eq!(failed.status, AgentPresetStoreStatus::Ready);
    assert_eq!(failed.current_value, "minimal");
    assert_eq!(failed.error.as_deref(), Some("read-only settings"));
}

#[test]
fn settings_store_distinguishes_empty_and_failed_rosters() {
    let empty = Transport::new(Vec::new());
    let controller = AgentPresetSettingsController::new(empty);
    futures::executor::block_on(controller.load());
    assert_eq!(
        controller.store().snapshot().status,
        AgentPresetStoreStatus::Unavailable
    );

    let failed = Transport::new(roster());
    *failed.list_error.borrow_mut() = Some("host down".to_owned());
    let controller = AgentPresetSettingsController::new(failed);
    futures::executor::block_on(controller.load());
    let state = controller.store().snapshot();
    assert_eq!(state.status, AgentPresetStoreStatus::Error);
    assert_eq!(state.error.as_deref(), Some("host down"));
}

#[test]
fn seat_stages_spends_once_and_publishes_applied_identity() {
    let transport = Transport::new(roster());
    let current = Rc::new(RefCell::new(None::<SeatSessionSummary>));
    let current_reader = current.clone();
    let applied = Rc::new(RefCell::new(Vec::new()));
    let applied_log = applied.clone();
    let controller = AgentPresetSeatController::new(
        transport.clone(),
        Rc::new(move || current_reader.borrow().clone()),
        Some(Rc::new(move |session, preset| {
            applied_log.borrow_mut().push((session, preset));
        })),
    );
    futures::executor::block_on(controller.load());
    assert_eq!(controller.store().snapshot().current, "standard");

    futures::executor::block_on(controller.select("minimal"));
    assert!(transport.selects.borrow().is_empty());
    assert_eq!(controller.store().snapshot().current, "minimal");

    *current.borrow_mut() = Some(SeatSessionSummary {
        id: SessionId::new("session-1"),
        blank: true,
        agent_preset: Some("standard".to_owned()),
    });
    futures::executor::block_on(controller.apply());
    futures::executor::block_on(controller.apply());
    assert_eq!(transport.selects.borrow().len(), 1);
    assert_eq!(applied.borrow().len(), 1);
    assert_eq!(applied.borrow()[0].0.as_str(), "session-1");
    assert_eq!(applied.borrow()[0].1, "minimal");
}

#[test]
fn seat_drops_unservable_stages_and_recovers_from_refusal() {
    let transport = Transport::new(roster());
    let current = Rc::new(RefCell::new(Some(SeatSessionSummary {
        id: SessionId::new("session-1"),
        blank: false,
        agent_preset: Some("standard".to_owned()),
    })));
    let current_reader = current.clone();
    let controller = AgentPresetSeatController::new(
        transport.clone(),
        Rc::new(move || current_reader.borrow().clone()),
        None,
    );
    futures::executor::block_on(controller.load());
    futures::executor::block_on(controller.select("minimal"));
    assert!(transport.selects.borrow().is_empty());

    *current.borrow_mut() = Some(SeatSessionSummary {
        id: SessionId::new("session-2"),
        blank: true,
        agent_preset: Some("standard".to_owned()),
    });
    *transport.select_error.borrow_mut() = Some("already started".to_owned());
    futures::executor::block_on(controller.select("minimal"));
    let state = controller.store().snapshot();
    assert!(!state.busy);
    assert_eq!(state.current, "standard");
    assert_eq!(state.error.as_deref(), Some("already started"));

    controller.stage("minimal", true);
    assert!(controller.store().snapshot().introduce);
    controller.introduced();
    assert!(!controller.store().snapshot().introduce);
}

#[test]
fn seat_refresh_preserves_stage_and_reports_roster_failure_without_erasing_options() {
    let transport = Transport::new(roster());
    let controller = AgentPresetSeatController::new(transport.clone(), Rc::new(|| None), None);
    futures::executor::block_on(controller.load());
    controller.stage("minimal", false);
    futures::executor::block_on(controller.load());
    assert_eq!(controller.store().snapshot().current, "minimal");

    *transport.list_error.borrow_mut() = Some("socket closed".to_owned());
    futures::executor::block_on(controller.load());
    let state = controller.store().snapshot();
    assert_eq!(state.error.as_deref(), Some("socket closed"));
    assert_eq!(state.options.len(), 2);
}

struct SectionFixture {
    roster: RefCell<RosterValue>,
    contents: RefCell<BTreeMap<String, PresetReadValue>>,
    calls: RefCell<Vec<String>>,
    failures: RefCell<BTreeMap<String, String>>,
}

impl SectionFixture {
    fn new() -> Rc<Self> {
        let mut rows = roster();
        rows[0].name = Some("Standard".to_owned());
        Rc::new(Self {
            roster: RefCell::new(RosterValue {
                presets: rows,
                authorable: true,
                has_document: false,
            }),
            contents: RefCell::new(BTreeMap::from([
                (
                    "standard".to_owned(),
                    PresetReadValue {
                        name: Some("Standard".to_owned()),
                        content: "- id: tool-bash\n".to_owned(),
                    },
                ),
                (
                    "minimal".to_owned(),
                    PresetReadValue {
                        name: None,
                        content: "[]\n".to_owned(),
                    },
                ),
            ])),
            calls: RefCell::new(Vec::new()),
            failures: RefCell::new(BTreeMap::new()),
        })
    }

    fn failure(&self, method: &str) -> Option<String> {
        self.failures.borrow().get(method).cloned()
    }
}

impl AgentPresetSectionTransport for SectionFixture {
    fn list(&self) -> LocalBoxFuture<'static, Result<RosterValue, String>> {
        self.calls.borrow_mut().push("list".to_owned());
        let result = self
            .failure("list")
            .map_or_else(|| Ok(self.roster.borrow().clone()), Err);
        async move { result }.boxed_local()
    }

    fn read(&self, id: String) -> LocalBoxFuture<'static, Result<PresetReadValue, String>> {
        self.calls.borrow_mut().push(format!("read:{id}"));
        let result = self.failure("read").map_or_else(
            || {
                self.contents
                    .borrow()
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| format!("unknown preset {id}"))
            },
            Err,
        );
        async move { result }.boxed_local()
    }

    fn copy(
        &self,
        from: String,
        id: String,
        name: Option<String>,
    ) -> LocalBoxFuture<'static, Result<(), String>> {
        self.calls.borrow_mut().push(format!(
            "copy:{from}:{id}:{}",
            name.as_deref().unwrap_or("")
        ));
        let result = self.failure("copy").map_or(Ok(()), Err);
        if result.is_ok() {
            let source = self.contents.borrow().get(&from).cloned();
            if let Some(source) = source {
                self.contents.borrow_mut().insert(
                    id.clone(),
                    PresetReadValue {
                        name: name.clone(),
                        content: source.content,
                    },
                );
                self.roster.borrow_mut().presets.push(RosterPreset {
                    id,
                    trust: PresetTrust::User,
                    is_default: false,
                    name,
                    description: None,
                    broken: None,
                });
            }
        }
        async move { result }.boxed_local()
    }

    fn open_document(
        &self,
        id: String,
    ) -> LocalBoxFuture<'static, Result<PresetOpenResult, String>> {
        self.calls.borrow_mut().push(format!("open:{id}"));
        let result = self.failure("open").map_or_else(
            || {
                if self.roster.borrow().has_document {
                    Ok(PresetOpenResult::Opened)
                } else {
                    Ok(PresetOpenResult::Path(format!("/presets/{id}")))
                }
            },
            Err,
        );
        async move { result }.boxed_local()
    }

    fn remove(&self, id: String) -> LocalBoxFuture<'static, Result<(), String>> {
        self.calls.borrow_mut().push(format!("remove:{id}"));
        let result = self.failure("remove").map_or(Ok(()), Err);
        if result.is_ok() {
            self.contents.borrow_mut().remove(&id);
            self.roster
                .borrow_mut()
                .presets
                .retain(|preset| preset.id != id);
        }
        async move { result }.boxed_local()
    }

    fn update_default(&self, id: String) -> LocalBoxFuture<'static, Result<(), String>> {
        self.calls.borrow_mut().push(format!("default:{id}"));
        let result = self.failure("default").map_or(Ok(()), Err);
        if result.is_ok() {
            for preset in &mut self.roster.borrow_mut().presets {
                preset.is_default = preset.id == id;
            }
        }
        async move { result }.boxed_local()
    }
}

#[test]
fn section_loads_views_reveals_and_prunes_host_paths() {
    let transport = SectionFixture::new();
    let controller = AgentPresetSectionController::new(transport.clone(), None);
    futures::executor::block_on(controller.load());
    let ready = controller.store().snapshot();
    assert_eq!(ready.status, AgentPresetSectionStatus::Ready);
    assert!(ready.authorable);
    assert!(!ready.has_document);
    assert_eq!(ready.rows.len(), 2);

    futures::executor::block_on(controller.view("standard"));
    assert_eq!(
        controller.store().snapshot().view.as_ref().unwrap().title,
        "Standard"
    );
    assert_eq!(
        controller.store().snapshot().view.as_ref().unwrap().content,
        "- id: tool-bash\n"
    );
    controller.close_view();
    assert!(controller.store().snapshot().view.is_none());

    futures::executor::block_on(controller.open_location("minimal"));
    assert_eq!(
        controller.store().snapshot().revealed_paths["minimal"],
        "/presets/minimal"
    );
    transport
        .roster
        .borrow_mut()
        .presets
        .retain(|preset| preset.id != "minimal");
    futures::executor::block_on(controller.load());
    assert!(
        !controller
            .store()
            .snapshot()
            .revealed_paths
            .contains_key("minimal")
    );
}

#[test]
fn section_copy_reloads_broadcasts_and_reveals_the_new_directory() {
    let transport = SectionFixture::new();
    let change_count = Rc::new(RefCell::new(0_u64));
    let callback_count = change_count.clone();
    let controller = AgentPresetSectionController::new(
        transport.clone(),
        Some(Rc::new(move || *callback_count.borrow_mut() += 1)),
    );
    futures::executor::block_on(controller.load());
    controller.begin_copy("standard");
    controller.set_copy_id("UPPER");
    futures::executor::block_on(controller.confirm_copy());
    assert!(
        !transport
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("copy:"))
    );

    controller.set_copy_id("my-copy");
    controller.set_copy_name("  My copy  ");
    futures::executor::block_on(controller.confirm_copy());
    let state = controller.store().snapshot();
    assert!(state.copy.is_none());
    assert!(state.rows.iter().any(|row| row.id == "my-copy"));
    assert_eq!(state.revealed_paths["my-copy"], "/presets/my-copy");
    assert_eq!(*change_count.borrow(), 1);
    assert!(
        transport
            .calls
            .borrow()
            .iter()
            .any(|call| call == "copy:standard:my-copy:My copy")
    );
}

#[test]
fn section_delete_default_and_failure_paths_preserve_host_authority() {
    let transport = SectionFixture::new();
    let change_count = Rc::new(RefCell::new(0_u64));
    let callback_count = change_count.clone();
    let controller = AgentPresetSectionController::new(
        transport.clone(),
        Some(Rc::new(move || *callback_count.borrow_mut() += 1)),
    );
    futures::executor::block_on(controller.load());
    controller.confirm_delete(Some("minimal"));
    futures::executor::block_on(controller.remove());
    assert!(
        !controller
            .store()
            .snapshot()
            .rows
            .iter()
            .any(|row| row.id == "minimal")
    );
    assert_eq!(*change_count.borrow(), 1);

    futures::executor::block_on(controller.make_default("standard"));
    assert!(
        controller
            .store()
            .snapshot()
            .rows
            .iter()
            .find(|row| row.id == "standard")
            .unwrap()
            .is_default
    );

    *transport.failures.borrow_mut() = BTreeMap::from([
        ("read".to_owned(), "no peeking".to_owned()),
        ("remove".to_owned(), "cannot remove".to_owned()),
    ]);
    futures::executor::block_on(controller.view("standard"));
    assert_eq!(
        controller.store().snapshot().error.as_deref(),
        Some("no peeking")
    );
    controller.confirm_delete(Some("standard"));
    futures::executor::block_on(controller.remove());
    let failed = controller.store().snapshot();
    assert!(!failed.deleting);
    assert!(failed.pending_delete.is_none());
    assert_eq!(failed.error.as_deref(), Some("cannot remove"));
}
