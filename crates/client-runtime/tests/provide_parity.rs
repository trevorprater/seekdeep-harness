//! Session standard-props provider roster and current projection parity.

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    SessionBinding, SessionProvideChannel, SessionProvideChannelHost, SessionProvideContribution,
    SessionProvideDescriptor, SessionProvideError, SessionProvideInfo,
};
use seekdeep_identity::SessionId;
use serde_json::{Value, json};

type Hook = Rc<String>;
type Projections = String;
type Binding = SessionBinding<Hook, Projections>;
type Info = SessionProvideInfo<Hook, Value, Projections>;
type Channel = SessionProvideChannel<Hook, Value, Projections>;

#[derive(Default)]
struct TestHost {
    channel: RefCell<Weak<Channel>>,
    bindings: RefCell<Vec<Binding>>,
    bundles: RefCell<Vec<Rc<Info>>>,
    current: Cell<Option<usize>>,
    rebuilds: Cell<u64>,
    reports: RefCell<Vec<String>>,
}

impl SessionProvideChannelHost<Hook, Value, Projections, ()> for TestHost {
    fn rebuild_bundles(&self, channel: &Channel) -> Result<(), SessionProvideError> {
        let bundles = self
            .bindings
            .borrow()
            .iter()
            .map(|binding| channel.materialize_info(binding))
            .collect::<Result<Vec<_>, _>>()?;
        *self.bundles.borrow_mut() = bundles;
        self.rebuilds.set(self.rebuilds.get() + 1);
        Ok(())
    }

    fn resolve_current(&self) -> Result<Rc<Info>, SessionProvideError> {
        let channel = self
            .channel
            .borrow()
            .upgrade()
            .ok_or_else(|| SessionProvideError::new("test channel unavailable"))?;
        Ok(self
            .current
            .get()
            .and_then(|index| self.bundles.borrow().get(index).cloned())
            .unwrap_or_else(|| channel.maybe_info()))
    }

    fn report_subscriber_failure(&self, message: &str) {
        self.reports.borrow_mut().push(message.to_owned());
    }
}

fn bench() -> (Rc<TestHost>, Rc<Channel>) {
    let host = Rc::new(TestHost::default());
    let channel = SessionProvideChannel::new(host.clone());
    *host.channel.borrow_mut() = Rc::downgrade(&channel);
    (host, channel)
}

fn binding(id: &str) -> Binding {
    SessionBinding {
        session_id: SessionId::new(id),
        session: Rc::new(format!("session:{id}")),
        projections: format!("projections:{id}"),
        payload: (),
    }
}

fn descriptor(
    hooks: &[&str],
    props: &[&str],
    resolve: impl Fn(&Binding) -> Result<SessionProvideContribution<Hook, Value>, SessionProvideError>
    + 'static,
) -> SessionProvideDescriptor<Hook, Value, Projections> {
    SessionProvideDescriptor {
        hooks: hooks.iter().map(ToString::to_string).collect(),
        props: props.iter().map(ToString::to_string).collect(),
        resolve: Rc::new(resolve),
    }
}

#[test]
fn built_in_session_hook_leads_static_and_definite_bundles() {
    let (_, channel) = bench();
    let absent = channel.maybe_info();
    assert!(absent.session_id.is_none());
    assert_eq!(
        absent.hooks.keys().map(String::as_str).collect::<Vec<_>>(),
        ["session"]
    );
    assert!(absent.hooks["session"].is_none());
    assert!(absent.projections.is_none());

    let binding = binding("s1");
    let info = channel.materialize_info(&binding).unwrap();
    assert_eq!(info.session_id.as_ref().map(SessionId::as_str), Some("s1"));
    assert!(Rc::ptr_eq(
        info.hooks["session"].as_ref().unwrap(),
        &binding.session
    ));
    assert_eq!(info.projections.as_deref(), Some("projections:s1"));
}

#[test]
fn roster_changes_rebuild_live_bundles_republish_current_and_dispose_repeatably() {
    let (host, channel) = bench();
    host.bindings.borrow_mut().push(binding("s1"));
    host.rebuild_bundles(&channel).unwrap();
    host.current.set(Some(0));
    channel.publish_current().unwrap();
    let before = channel.current_snapshot();
    let ticks = Rc::new(Cell::new(0));
    let observed = ticks.clone();
    let _subscription = channel.subscribe_current(Rc::new(move || {
        observed.set(observed.get() + 1);
        Ok(())
    }));

    let extra = Rc::new("extra-source".to_owned());
    let expected = extra.clone();
    let registration = channel
        .provide(descriptor(&["extra"], &["marker"], move |_| {
            Ok(SessionProvideContribution {
                hooks: IndexMap::from([("extra".to_owned(), Some(extra.clone()))]),
                props: IndexMap::from([("marker".to_owned(), Some(json!(7)))]),
            })
        }))
        .unwrap();
    let added = channel.current_snapshot();
    assert!(!Rc::ptr_eq(&before, &added));
    assert!(Rc::ptr_eq(
        added.hooks["extra"].as_ref().unwrap(),
        &expected
    ));
    assert_eq!(added.props["marker"], Some(json!(7)));
    assert_eq!(ticks.get(), 1);
    let absent = channel.maybe_info();
    assert!(absent.hooks.contains_key("extra"));
    assert!(absent.hooks["extra"].is_none());

    registration.dispose().unwrap();
    let removed = channel.current_snapshot();
    assert!(!removed.hooks.contains_key("extra"));
    assert_eq!(ticks.get(), 2);
    let rebuilds = host.rebuilds.get();
    registration.dispose().unwrap();
    assert_eq!(host.rebuilds.get(), rebuilds + 1);
    assert_eq!(ticks.get(), 3);
}

#[test]
fn invalid_live_provider_rolls_back_without_poisoning_the_previous_roster() {
    let (host, channel) = bench();
    host.bindings.borrow_mut().push(binding("s1"));
    host.rebuild_bundles(&channel).unwrap();

    let error = channel
        .provide(descriptor(&["extra"], &[], |_| {
            Ok(SessionProvideContribution {
                hooks: IndexMap::from([("other".to_owned(), Some(Rc::new("x".to_owned())))]),
                props: IndexMap::new(),
            })
        }))
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "sessions.provide: undeclared hook \"other\""
    );
    assert_eq!(
        channel
            .maybe_info()
            .hooks
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["session"]
    );
    assert_eq!(host.bundles.borrow()[0].hooks.len(), 1);

    let error = channel
        .provide(descriptor(&["missing"], &[], |_| {
            Ok(SessionProvideContribution::default())
        }))
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "sessions.provide: missing hook \"missing\""
    );
    let error = channel
        .provide(descriptor(&["session"], &[], |_| {
            Ok(SessionProvideContribution::default())
        }))
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "sessions.provide: duplicate hook \"session\""
    );

    let error = channel
        .provide(descriptor(&[], &["marker"], |_| {
            Ok(SessionProvideContribution {
                hooks: IndexMap::new(),
                props: IndexMap::from([("other".to_owned(), Some(json!(1)))]),
            })
        }))
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "sessions.provide: undeclared prop \"other\""
    );
    let error = channel
        .provide(descriptor(&[], &["marker"], |_| {
            Ok(SessionProvideContribution::default())
        }))
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "sessions.provide: missing prop \"marker\""
    );
    let registration = channel
        .provide(descriptor(&[], &["marker"], |_| {
            Ok(SessionProvideContribution {
                hooks: IndexMap::new(),
                props: IndexMap::from([("marker".to_owned(), Some(Value::Null))]),
            })
        }))
        .unwrap();
    let error = channel
        .provide(descriptor(&[], &["marker"], |_| {
            Ok(SessionProvideContribution::default())
        }))
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "sessions.provide: duplicate prop \"marker\""
    );
    registration.dispose().unwrap();
}

#[test]
fn current_publication_dedupes_identity_unsubscribes_and_contains_failures() {
    let (host, channel) = bench();
    host.bindings.borrow_mut().push(binding("s1"));
    host.rebuild_bundles(&channel).unwrap();
    host.current.set(Some(0));
    let later = Rc::new(Cell::new(0));
    let failing = channel.subscribe_current(Rc::new(|| Err("render failed".to_owned())));
    let observed = later.clone();
    let healthy = channel.subscribe_current(Rc::new(move || {
        observed.set(observed.get() + 1);
        Ok(())
    }));

    channel.publish_current().unwrap();
    assert_eq!(host.reports.borrow().as_slice(), ["render failed"]);
    assert_eq!(later.get(), 1);
    channel.publish_current().unwrap();
    assert_eq!(later.get(), 1);
    failing.dispose();
    healthy.dispose();
    host.current.set(None);
    channel.publish_current().unwrap();
    assert_eq!(later.get(), 1);
}
