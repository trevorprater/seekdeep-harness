//! Portable locale registry, Host preference, dictionary, and row-store parity.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use indexmap::IndexMap;
use seekdeep_client_locale::{
    FALLBACK_LOCALE, LanguageOptionRow, LanguageRowStore, LocaleDictionary, LocaleDisposer,
    LocaleHostScope, LocaleId, LocaleRuntime, LocaleSettings, chinese_common_dictionary,
    detect_browser_locale, english_common_dictionary, language_settings_dictionaries,
};
use serde_json::json;

fn dictionary(entries: &[(&str, &str)]) -> LocaleDictionary {
    entries
        .iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
}

#[derive(Default)]
struct Host {
    section: RefCell<Option<LocaleSettings>>,
    writes: RefCell<Vec<LocaleId>>,
    listeners: RefCell<HashMap<u64, Rc<dyn Fn()>>>,
    next: Cell<u64>,
}

impl Host {
    fn callbacks(self: &Rc<Self>) -> LocaleHostScope {
        let snapshot = self.clone();
        let writer = self.clone();
        let subscriber = self.clone();
        LocaleHostScope {
            snapshot: Rc::new(move || snapshot.section.borrow().clone()),
            set_preference: Rc::new(move |locale| writer.writes.borrow_mut().push(locale)),
            subscribe: Rc::new(move |listener| {
                let id = subscriber.next.get();
                subscriber.next.set(id + 1);
                subscriber.listeners.borrow_mut().insert(id, listener);
                let weak = Rc::downgrade(&subscriber);
                LocaleDisposer::new(move || {
                    if let Some(host) = weak.upgrade() {
                        host.listeners.borrow_mut().remove(&id);
                    }
                })
            }),
        }
    }

    fn publish(&self, settings: LocaleSettings) {
        *self.section.borrow_mut() = Some(settings);
        for listener in self
            .listeners
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>()
        {
            listener();
        }
    }
}

fn runtime(host: Option<&Rc<Host>>) -> (LocaleRuntime, Rc<RefCell<Vec<LocaleId>>>) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let event_sink = events.clone();
    (
        LocaleRuntime::new(
            FALLBACK_LOCALE,
            host.map(Host::callbacks),
            move |snapshot| event_sink.borrow_mut().push(snapshot.active),
            |_| {},
        ),
        events,
    )
}

#[test]
fn translates_active_fallback_common_key_and_parameters() {
    let (locale, _) = runtime(None);
    locale
        .register("common", "zh", dictionary(&[("retry", "重试")]))
        .unwrap();
    locale
        .register("common", "en", dictionary(&[("retry", "Retry")]))
        .unwrap();
    locale
        .register(
            "ns",
            "zh",
            dictionary(&[("hello", "你好，{name}！第 {n} 次"), ("onlyZh", "仅中文")]),
        )
        .unwrap();
    locale
        .register("ns", "en", dictionary(&[("hello", "Hello, {name}!")]))
        .unwrap();
    let t = locale.bind("ns");
    let parameters = IndexMap::from([
        ("name".into(), json!("世界")),
        (
            "n".into(),
            serde_json::Value::Number(serde_json::Number::from_f64(2.0).unwrap()),
        ),
    ]);
    assert_eq!(t("hello", Some(&parameters)), "你好，世界！第 2 次");
    assert_eq!(t("retry", None), "重试");
    locale.set_locale("en").unwrap();
    assert_eq!(t("hello", Some(&parameters)), "Hello, 世界!");
    assert_eq!(t("onlyZh", None), "仅中文");
    assert_eq!(t("missing.key", None), "missing.key");
}

#[test]
fn bound_identity_duplicate_seats_and_stale_disposer_match_source() {
    let (locale, _) = runtime(None);
    assert!(Rc::ptr_eq(&locale.bind("a"), &locale.bind("a")));
    assert!(!Rc::ptr_eq(&locale.bind("a"), &locale.bind("b")));
    let first = locale
        .register("ns", "zh", dictionary(&[("k", "v1")]))
        .unwrap();
    assert!(
        locale
            .register("ns", "zh", dictionary(&[("k", "v2")]))
            .is_err()
    );
    first.dispose();
    let second = locale
        .register("ns", "zh", dictionary(&[("k", "v2")]))
        .unwrap();
    first.dispose();
    assert_eq!(locale.bind("ns")("k", None), "v2");
    second.dispose();
    assert_eq!(locale.bind("ns")("k", None), "k");
}

#[test]
fn snapshots_subscribers_and_host_preference_preserve_lifecycle() {
    let host = Rc::new(Host::default());
    let (locale, events) = runtime(Some(&host));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let seen_sink = seen.clone();
    let observed = locale.clone();
    let subscription = locale.subscribe(Rc::new(move || {
        seen_sink.borrow_mut().push(observed.snapshot().revision);
    }));
    let initial = locale.snapshot();
    let registration = locale
        .register("ns", "zh", dictionary(&[("k", "v")]))
        .unwrap();
    assert_eq!(locale.snapshot().revision, initial.revision + 1);
    assert!(Rc::ptr_eq(&initial.locales, &locale.snapshot().locales));
    locale.set_locale("en").unwrap();
    assert_eq!(*events.borrow(), [LocaleId::En]);
    assert_eq!(*host.writes.borrow(), [LocaleId::En]);
    assert_eq!(*seen.borrow(), [1, 2]);
    subscription.dispose();
    registration.dispose();
    assert_eq!(*seen.borrow(), [1, 2]);
    host.publish(LocaleSettings {
        preference: Some(LocaleId::Zh),
    });
    assert_eq!(locale.snapshot().active, LocaleId::Zh);
    assert_eq!(host.writes.borrow().len(), 1);
    locale.dispose();
    assert!(host.listeners.borrow().is_empty());
}

#[test]
fn same_locale_is_a_noop_unknown_ids_fail_and_host_adoption_never_writes_back() {
    let host = Rc::new(Host::default());
    host.publish(LocaleSettings {
        preference: Some(LocaleId::En),
    });
    let (locale, events) = runtime(Some(&host));
    assert_eq!(locale.snapshot().active, LocaleId::En);
    assert!(host.writes.borrow().is_empty());
    assert_eq!(*events.borrow(), [LocaleId::En]);
    locale.set_locale("en").unwrap();
    assert_eq!(*events.borrow(), [LocaleId::En]);
    assert!(host.writes.borrow().is_empty());
    assert!(
        locale
            .set_locale("fr")
            .unwrap_err()
            .to_string()
            .contains("not registered")
    );
    host.publish(LocaleSettings { preference: None });
    assert_eq!(locale.snapshot().active, LocaleId::Zh);
    assert_eq!(*events.borrow(), [LocaleId::En, LocaleId::Zh]);
    assert!(host.writes.borrow().is_empty());
}

#[test]
fn throwing_subscriber_isolated_and_disposer_republishes_once() {
    let errors = Rc::new(RefCell::new(Vec::new()));
    let error_sink = errors.clone();
    let locale = LocaleRuntime::new(
        FALLBACK_LOCALE,
        None,
        |_| {},
        move |message| error_sink.borrow_mut().push(message),
    );
    let _throwing = locale.subscribe(Rc::new(|| panic!("boom")));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let seen_sink = seen.clone();
    let observed = locale.clone();
    let _observing = locale.subscribe(Rc::new(move || {
        seen_sink.borrow_mut().push(observed.snapshot().revision);
    }));
    let registration = locale
        .register("ns", "zh", dictionary(&[("k", "v")]))
        .unwrap();
    assert_eq!(*seen.borrow(), [1]);
    assert_eq!(errors.borrow().len(), 1);
    let before = locale.snapshot().revision;
    registration.dispose();
    assert_eq!(locale.snapshot().revision, before + 1);
    registration.dispose();
    assert_eq!(locale.snapshot().revision, before + 1);
}

#[test]
fn browser_language_detection_uses_primary_subtags_and_node_fallback() {
    assert_eq!(
        detect_browser_locale(true, Some(&["en-GB".into(), "zh-CN".into()]), "en-GB"),
        Some(LocaleId::En)
    );
    assert_eq!(
        detect_browser_locale(true, Some(&["zh-Hant-TW".into()]), "zh-Hant-TW"),
        Some(LocaleId::Zh)
    );
    assert_eq!(
        detect_browser_locale(true, Some(&["fr-FR".into(), "en-US".into()]), "fr-FR"),
        Some(LocaleId::En)
    );
    assert_eq!(
        detect_browser_locale(true, None, "en-US"),
        Some(LocaleId::En)
    );
    assert_eq!(detect_browser_locale(false, None, "en-US"), None);
}

#[test]
fn language_row_store_initializes_and_rejects_stale_revisions() {
    let store = LanguageRowStore::default();
    assert_eq!(store.snapshot().revision, -1);
    let options = vec![
        LanguageOptionRow {
            id: "zh".into(),
            label: "中文".into(),
        },
        LanguageOptionRow {
            id: "en".into(),
            label: "English".into(),
        },
    ];
    store.sync("en".into(), options.clone(), 5);
    store.sync("zh".into(), options.clone(), 4);
    store.sync("zh".into(), options, 5);
    assert_eq!(store.snapshot().active, "en");
    assert_eq!(store.snapshot().revision, 5);
}

#[test]
fn shipped_dictionaries_are_balanced_and_locale_order_is_stable() {
    let chinese = chinese_common_dictionary();
    let english = english_common_dictionary();
    assert_eq!(chinese.len(), 24);
    assert_eq!(english.len(), 24);
    assert!(chinese.keys().eq(english.keys()));
    assert_eq!(
        chinese.get("load.failed").map(String::as_str),
        Some("加载失败")
    );
    assert_eq!(
        english.get("truncated").map(String::as_str),
        Some("Truncated")
    );
    let settings = language_settings_dictionaries();
    assert_eq!(settings[0].0, "zh");
    assert_eq!(settings[1].0, "en");
    let (locale, _) = runtime(None);
    assert_eq!(
        locale
            .snapshot()
            .locales
            .iter()
            .map(|locale| (locale.id.as_str(), locale.label))
            .collect::<Vec<_>>(),
        [("zh", "中文"), ("en", "English")]
    );
}

#[test]
fn dropping_runtime_releases_host_subscription_after_failed_or_unowned_assembly() {
    let host = Rc::new(Host::default());
    {
        let (_locale, _) = runtime(Some(&host));
        assert_eq!(host.listeners.borrow().len(), 1);
    }
    assert!(host.listeners.borrow().is_empty());
}
