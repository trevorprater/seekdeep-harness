//! Portable dictionary registry, locale snapshots, preference adoption, and translation.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    rc::{Rc, Weak},
    sync::LazyLock,
};

use indexmap::IndexMap;
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Settings namespace owned by the locale plugin.
pub const LOCALE_SETTINGS_NAMESPACE: &str = "locale";
/// Field carrying an explicit locale selection.
pub const LOCALE_PREFERENCE_FIELD: &str = "preference";
/// Fallback locale consulted after the active locale misses.
pub const FALLBACK_LOCALE: LocaleId = LocaleId::Zh;
/// Shared namespace for shell-level text.
pub const COMMON_NAMESPACE: &str = "common";
/// Namespace owning the feature's settings-row copy.
pub const SETTINGS_NAMESPACE: &str = "settings.locale";

static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{(\w+)\}").expect("valid regex"));

/// Shipped locale identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocaleId {
    /// Simplified Chinese.
    Zh,
    /// English.
    En,
}

impl LocaleId {
    /// Stable persisted locale tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }

    /// Parses an exact shipped locale identifier.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "zh" => Some(Self::Zh),
            "en" => Some(Self::En),
            _ => None,
        }
    }
}

/// Durable locale section shared by the Host and browser scope.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocaleSettings {
    /// Explicit locale selection; absence delegates to the browser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preference: Option<LocaleId>,
}

/// One selectable locale with a self-described label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocaleDefinition {
    /// Persisted locale id.
    pub id: LocaleId,
    /// Display label in its own language.
    pub label: &'static str,
}

/// Immutable locale state published on every change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocaleSnapshot {
    /// Active locale id.
    pub active: LocaleId,
    /// Selectable locales in display order; identity is stable across revisions.
    pub locales: Rc<Vec<LocaleDefinition>>,
    /// Monotonic registry or active-locale revision.
    pub revision: u64,
}

/// Flat locale dictionary.
pub type LocaleDictionary = IndexMap<String, String>;
/// Template interpolation values.
pub type TranslateParameters = IndexMap<String, Value>;
/// Stable bound translation function.
pub type Translate = dyn Fn(&str, Option<&TranslateParameters>) -> String;
/// Locale revision listener.
pub type LocaleListener = Rc<dyn Fn()>;
/// Host-scope subscription function.
pub type LocaleSubscribe = Rc<dyn Fn(LocaleListener) -> LocaleDisposer>;
type DisposalCallback = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// Idempotent locale-owned disposer.
#[derive(Clone)]
pub struct LocaleDisposer(DisposalCallback);

impl LocaleDisposer {
    /// Builds a disposer around one exact cleanup.
    #[must_use]
    pub fn new(callback: impl FnOnce() + 'static) -> Self {
        Self(Rc::new(RefCell::new(Some(Box::new(callback)))))
    }

    /// Runs cleanup at most once.
    pub fn dispose(&self) {
        if let Some(callback) = self.0.borrow_mut().take() {
            callback();
        }
    }
}

/// Host preference scope callbacks consumed by the portable runtime.
#[derive(Clone)]
pub struct LocaleHostScope {
    /// Current accepted section; `None` means not materialized yet.
    pub snapshot: Rc<dyn Fn() -> Option<LocaleSettings>>,
    /// Fire-and-forget durable preference write.
    pub set_preference: Rc<dyn Fn(LocaleId)>,
    /// Live section subscription.
    pub subscribe: LocaleSubscribe,
}

struct LocaleInner {
    dictionaries: RefCell<HashMap<String, HashMap<String, Rc<LocaleDictionary>>>>,
    bound: RefCell<HashMap<String, Rc<Translate>>>,
    snapshot: RefCell<Rc<LocaleSnapshot>>,
    listeners: RefCell<IndexMap<u64, LocaleListener>>,
    next_listener: Cell<u64>,
    host: Option<LocaleHostScope>,
    host_subscription: RefCell<Option<LocaleDisposer>>,
    provisional: LocaleId,
    emit_change: Rc<dyn Fn(Rc<LocaleSnapshot>)>,
    report_listener_error: Rc<dyn Fn(String)>,
}

impl Drop for LocaleInner {
    fn drop(&mut self) {
        if let Some(subscription) = self.host_subscription.get_mut().take() {
            subscription.dispose();
        }
    }
}

/// Dictionary registry plus Host-backed locale preference.
#[derive(Clone)]
pub struct LocaleRuntime {
    inner: Rc<LocaleInner>,
}

impl LocaleRuntime {
    /// Creates one locale runtime with an injected provisional browser choice.
    #[must_use]
    pub fn new(
        provisional: LocaleId,
        host: Option<LocaleHostScope>,
        emit_change: impl Fn(Rc<LocaleSnapshot>) + 'static,
        report_listener_error: impl Fn(String) + 'static,
    ) -> Self {
        let locales = Rc::new(vec![
            LocaleDefinition {
                id: LocaleId::Zh,
                label: "中文",
            },
            LocaleDefinition {
                id: LocaleId::En,
                label: "English",
            },
        ]);
        let runtime = Self {
            inner: Rc::new(LocaleInner {
                dictionaries: RefCell::new(HashMap::new()),
                bound: RefCell::new(HashMap::new()),
                snapshot: RefCell::new(Rc::new(LocaleSnapshot {
                    active: provisional,
                    locales,
                    revision: 0,
                })),
                listeners: RefCell::new(IndexMap::new()),
                next_listener: Cell::new(0),
                host: host.clone(),
                host_subscription: RefCell::new(None),
                provisional,
                emit_change: Rc::new(emit_change),
                report_listener_error: Rc::new(report_listener_error),
            }),
        };
        if let Some(host) = host {
            let weak = Rc::downgrade(&runtime.inner);
            let subscription = (host.subscribe)(Rc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    Self { inner }.adopt_host();
                }
            }));
            *runtime.inner.host_subscription.borrow_mut() = Some(subscription);
            runtime.adopt_host();
        }
        runtime
    }

    /// Current immutable locale snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Rc<LocaleSnapshot> {
        self.inner.snapshot.borrow().clone()
    }

    /// Subscribes to every snapshot revision.
    #[must_use]
    pub fn subscribe(&self, listener: LocaleListener) -> LocaleDisposer {
        let id = self.inner.next_listener.get();
        self.inner.next_listener.set(id.saturating_add(1));
        self.inner.listeners.borrow_mut().insert(id, listener);
        let weak = Rc::downgrade(&self.inner);
        LocaleDisposer::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.listeners.borrow_mut().shift_remove(&id);
            }
        })
    }

    /// Switches the active locale and writes the Host preference when mounted.
    ///
    /// # Errors
    ///
    /// Rejects unknown locale identifiers.
    pub fn set_locale(&self, id: &str) -> anyhow::Result<()> {
        let locale = LocaleId::parse(id)
            .ok_or_else(|| anyhow::anyhow!("locale {id:?} is not registered"))?;
        if self.snapshot().active == locale {
            return Ok(());
        }
        self.publish(locale, true);
        if let Some(host) = &self.inner.host {
            (host.set_preference)(locale);
        }
        Ok(())
    }

    /// Registers one namespace/locale dictionary.
    ///
    /// # Errors
    ///
    /// Rejects an occupied namespace/locale pair.
    pub fn register(
        &self,
        namespace: impl Into<String>,
        locale: impl Into<String>,
        dictionary: LocaleDictionary,
    ) -> anyhow::Result<LocaleRegistration> {
        self.register_many(namespace, [(locale.into(), dictionary)])
    }

    /// Registers all supplied locale dictionaries atomically under one namespace.
    ///
    /// # Errors
    ///
    /// Rejects occupied or duplicate locale seats before installing any dictionary.
    pub fn register_many(
        &self,
        namespace: impl Into<String>,
        dictionaries: impl IntoIterator<Item = (String, LocaleDictionary)>,
    ) -> anyhow::Result<LocaleRegistration> {
        let namespace = namespace.into();
        let dictionaries = dictionaries.into_iter().collect::<Vec<_>>();
        let mut seen = HashSet::new();
        {
            let mut all = self.inner.dictionaries.borrow_mut();
            let locales = all.entry(namespace.clone()).or_default();
            for (locale, _) in &dictionaries {
                anyhow::ensure!(
                    seen.insert(locale.clone()) && !locales.contains_key(locale),
                    "locale namespace {namespace:?} already has locale {locale:?}"
                );
            }
        }
        let pairs = dictionaries
            .into_iter()
            .map(|(locale, dictionary)| (locale, Rc::new(dictionary)))
            .collect::<Vec<_>>();
        {
            let mut all = self.inner.dictionaries.borrow_mut();
            let locales = all.entry(namespace.clone()).or_default();
            for (locale, dictionary) in &pairs {
                locales.insert(locale.clone(), dictionary.clone());
            }
        }
        self.publish(self.snapshot().active, false);
        Ok(LocaleRegistration {
            runtime: Rc::downgrade(&self.inner),
            namespace,
            pairs,
            active: Cell::new(true),
        })
    }

    /// Returns one stable translation function per namespace.
    #[must_use]
    pub fn bind(&self, namespace: impl Into<String>) -> Rc<Translate> {
        let namespace = namespace.into();
        if let Some(bound) = self.inner.bound.borrow().get(&namespace) {
            return bound.clone();
        }
        let weak = Rc::downgrade(&self.inner);
        let bound_namespace = namespace.clone();
        let translate: Rc<Translate> = Rc::new(move |key, parameters| {
            weak.upgrade().map_or_else(
                || key.to_owned(),
                |inner| Self { inner }.translate(&bound_namespace, key, parameters),
            )
        });
        self.inner
            .bound
            .borrow_mut()
            .insert(namespace, translate.clone());
        translate
    }

    /// Releases the Host scope subscription.
    pub fn dispose(&self) {
        if let Some(subscription) = self.inner.host_subscription.borrow_mut().take() {
            subscription.dispose();
        }
    }

    fn adopt_host(&self) {
        let Some(host) = &self.inner.host else {
            return;
        };
        let Some(settings) = (host.snapshot)() else {
            return;
        };
        let target = settings.preference.unwrap_or(self.inner.provisional);
        if self.snapshot().active != target {
            self.publish(target, true);
        }
    }

    fn publish(&self, active: LocaleId, locale_changed: bool) {
        let previous = self.snapshot();
        let snapshot = Rc::new(LocaleSnapshot {
            active,
            locales: previous.locales.clone(),
            revision: previous.revision.saturating_add(1),
        });
        *self.inner.snapshot.borrow_mut() = snapshot.clone();
        if locale_changed {
            (self.inner.emit_change)(snapshot);
        }
        let listeners = self
            .inner
            .listeners
            .borrow()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            if catch_unwind(AssertUnwindSafe(|| listener())).is_err() {
                (self.inner.report_listener_error)("locale subscriber crashed".into());
            }
        }
    }

    fn translate(
        &self,
        namespace: &str,
        key: &str,
        parameters: Option<&TranslateParameters>,
    ) -> String {
        let template = self
            .lookup(namespace, key)
            .or_else(|| {
                (namespace != COMMON_NAMESPACE)
                    .then(|| self.lookup(COMMON_NAMESPACE, key))
                    .flatten()
            })
            .unwrap_or_else(|| key.to_owned());
        let Some(parameters) = parameters else {
            return template;
        };
        PLACEHOLDER
            .replace_all(&template, |captures: &Captures<'_>| {
                let name = &captures[1];
                parameters
                    .get(name)
                    .map_or_else(|| captures[0].to_owned(), javascript_string)
            })
            .into_owned()
    }

    fn lookup(&self, namespace: &str, key: &str) -> Option<String> {
        let dictionaries = self.inner.dictionaries.borrow();
        let locales = dictionaries.get(namespace)?;
        locales
            .get(self.snapshot().active.as_str())
            .and_then(|dictionary| dictionary.get(key))
            .or_else(|| {
                locales
                    .get(FALLBACK_LOCALE.as_str())
                    .and_then(|dictionary| dictionary.get(key))
            })
            .cloned()
    }
}

/// Lifecycle handle for one namespace dictionary registration.
pub struct LocaleRegistration {
    runtime: Weak<LocaleInner>,
    namespace: String,
    pairs: Vec<(String, Rc<LocaleDictionary>)>,
    active: Cell<bool>,
}

impl LocaleRegistration {
    /// Removes only dictionaries still owned by this registration.
    pub fn dispose(&self) {
        if !self.active.replace(false) {
            return;
        }
        let Some(inner) = self.runtime.upgrade() else {
            return;
        };
        let mut removed = false;
        if let Some(locales) = inner.dictionaries.borrow_mut().get_mut(&self.namespace) {
            for (locale, dictionary) in &self.pairs {
                if locales
                    .get(locale)
                    .is_some_and(|current| Rc::ptr_eq(current, dictionary))
                {
                    locales.remove(locale);
                    removed = true;
                }
            }
        }
        if removed {
            let active = inner.snapshot.borrow().active;
            LocaleRuntime { inner }.publish(active, false);
        }
    }
}

/// Detects the first shipped browser language by primary subtag.
#[must_use]
pub fn detect_browser_locale(
    browser: bool,
    languages: Option<&[String]>,
    language: &str,
) -> Option<LocaleId> {
    if !browser {
        return None;
    }
    languages
        .into_iter()
        .flatten()
        .map(String::as_str)
        .chain(std::iter::once(language))
        .find_map(|tag| {
            let primary = tag.split('-').next().unwrap_or(tag).to_ascii_lowercase();
            LocaleId::parse(&primary)
        })
}

fn javascript_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => javascript_number(value),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    javascript_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

fn javascript_number(value: &serde_json::Number) -> String {
    if let Some(value) = value.as_i64() {
        return value.to_string();
    }
    if let Some(value) = value.as_u64() {
        return value.to_string();
    }
    let value = value.as_f64().unwrap_or(f64::NAN);
    if value == 0.0 {
        return "0".into();
    }
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e21 {
        return format!("{value:.0}");
    }
    value.to_string()
}

/// Shipped Chinese common vocabulary.
#[must_use]
pub fn chinese_common_dictionary() -> LocaleDictionary {
    common_dictionary([
        ("ok", "确定"),
        ("cancel", "取消"),
        ("close", "关闭"),
        ("copy", "复制"),
        ("copied", "复制成功"),
        ("retry", "重试"),
        ("loading", "加载中…"),
        ("load.failed", "加载失败"),
        ("submit", "提交"),
        ("submitting", "正在提交…"),
        ("next", "下一步"),
        ("previous", "上一步"),
        ("skip", "跳过"),
        ("delete", "删除"),
        ("edit", "编辑"),
        ("save", "保存"),
        ("search", "搜索"),
        ("more", "更多"),
        ("collapse", "收起"),
        ("expand", "展开"),
        ("back", "返回"),
        ("unknown", "未知"),
        ("none", "无"),
        ("truncated", "已截断"),
    ])
}

/// Shipped English common vocabulary.
#[must_use]
pub fn english_common_dictionary() -> LocaleDictionary {
    common_dictionary([
        ("ok", "OK"),
        ("cancel", "Cancel"),
        ("close", "Close"),
        ("copy", "Copy"),
        ("copied", "Copied"),
        ("retry", "Retry"),
        ("loading", "Loading…"),
        ("load.failed", "Failed to load"),
        ("submit", "Submit"),
        ("submitting", "Submitting…"),
        ("next", "Next"),
        ("previous", "Previous"),
        ("skip", "Skip"),
        ("delete", "Delete"),
        ("edit", "Edit"),
        ("save", "Save"),
        ("search", "Search"),
        ("more", "More"),
        ("collapse", "Collapse"),
        ("expand", "Expand"),
        ("back", "Back"),
        ("unknown", "Unknown"),
        ("none", "None"),
        ("truncated", "Truncated"),
    ])
}

/// Language-row settings copy for both shipped locales.
#[must_use]
pub fn language_settings_dictionaries() -> [(String, LocaleDictionary); 2] {
    [
        ("zh".into(), common_dictionary([("language.title", "语言")])),
        (
            "en".into(),
            common_dictionary([("language.title", "Language")]),
        ),
    ]
}

fn common_dictionary<const N: usize>(entries: [(&str, &str); N]) -> LocaleDictionary {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}
