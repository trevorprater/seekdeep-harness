//! Target-portable staged plugin-card form state.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use seekdeep_client_runtime::{SnapshotStore, StoreFlushMode, StoreFlushScheduler, StoreLogger};
use seekdeep_client_settings_contract::{
    ClientSettingsDisposer, ClientSettingsScope, ClientSettingsScopeSnapshot, ClientSettingsStatus,
};
use serde::Serialize;
use serde_json::{Number, Value};

/// One field's planned durable mutation.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldWrite {
    /// Set one JSON-compatible value.
    Set(Value),
    /// Clear the user-layer field so it re-inherits.
    Clear,
}

/// Numeric field conversion contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberField {
    /// Field name inside the Settings namespace.
    pub field: String,
}

impl NumberField {
    /// Formats numeric stored values, otherwise the empty draft.
    #[must_use]
    pub fn format(&self, value: &Value) -> String {
        value.as_f64().map_or_else(String::new, javascript_number)
    }

    /// Empty clears; finite JavaScript-compatible numeric text sets; malformed blocks save.
    #[must_use]
    pub fn parse(&self, text: &str) -> Option<FieldWrite> {
        let trimmed = trim_ecmascript_whitespace(text);
        if trimmed.is_empty() {
            return Some(FieldWrite::Clear);
        }
        parse_js_number(trimmed)
            .filter(|value| value.is_finite())
            .and_then(Number::from_f64)
            .map(|value| FieldWrite::Set(Value::Number(value)))
    }
}

/// Free-text field conversion contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextField {
    /// Field name inside the Settings namespace.
    pub field: String,
}

impl TextField {
    /// Formats string stored values, otherwise the empty draft.
    #[must_use]
    pub fn format(&self, value: &Value) -> String {
        value.as_str().map_or_else(String::new, ToOwned::to_owned)
    }

    /// Empty clears; other text is trimmed before writing.
    #[must_use]
    pub fn parse(&self, text: &str) -> FieldWrite {
        let trimmed = trim_ecmascript_whitespace(text);
        if trimmed.is_empty() {
            FieldWrite::Clear
        } else {
            FieldWrite::Set(Value::String(trimmed.to_owned()))
        }
    }
}

/// One section-field conversion grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardFieldSpec {
    /// JavaScript-number field.
    Number(NumberField),
    /// Trimmed free-text field.
    Text(TextField),
}

impl CardFieldSpec {
    fn field(&self) -> &str {
        match self {
            Self::Number(spec) => &spec.field,
            Self::Text(spec) => &spec.field,
        }
    }

    fn format(&self, value: &Value) -> String {
        match self {
            Self::Number(spec) => spec.format(value),
            Self::Text(spec) => spec.format(value),
        }
    }

    fn parse(&self, text: &str) -> Option<FieldWrite> {
        match self {
            Self::Number(spec) => spec.parse(text),
            Self::Text(spec) => Some(spec.parse(text)),
        }
    }
}

impl From<NumberField> for CardFieldSpec {
    fn from(value: NumberField) -> Self {
        Self::Number(value)
    }
}

impl From<TextField> for CardFieldSpec {
    fn from(value: TextField) -> Self {
        Self::Text(value)
    }
}

/// Creates a number field spec.
#[must_use]
pub fn number_field(field: impl Into<String>) -> NumberField {
    NumberField {
        field: field.into(),
    }
}

/// Creates a text field spec.
#[must_use]
pub fn text_field(field: impl Into<String>) -> TextField {
    TextField {
        field: field.into(),
    }
}

/// Async write owned outside the Settings namespace, such as a credential.
pub type CardSecretWriter = Rc<dyn Fn(String) -> LocalBoxFuture<'static, bool>>;

/// One write-only field staged with the Settings fields.
#[derive(Clone)]
pub struct CardSecretSpec {
    /// Field name inside the card form.
    pub field: String,
    /// Host-owned write and read-back transaction.
    pub write: CardSecretWriter,
}

/// One field as a card renders it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardFieldState {
    /// Draft text.
    pub text: String,
    /// Whether saving leaves a user-layer field.
    pub overridden: bool,
    /// Whether the draft cannot be written.
    pub invalid: bool,
}

/// Form state shared by every plugin card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Exact source card shell is six independent UI facts.
pub struct CardShell {
    /// Whether the namespace is served.
    pub available: bool,
    /// Whether the Settings document accepts writes.
    pub writable: bool,
    /// Whether a save has work.
    pub dirty: bool,
    /// Whether any staged field blocks saving.
    pub invalid: bool,
    /// Whether one save is crossing the boundary.
    pub saving: bool,
    /// Whether the last save did not land.
    pub failed: bool,
}

#[derive(Clone)]
struct StagedEdit {
    text: String,
    clear: bool,
}

enum PlannedWrite {
    Invalid,
    Set {
        field: String,
        value: Value,
    },
    Clear {
        field: String,
    },
    Secret {
        write: CardSecretWriter,
        value: String,
    },
}

struct ImmediateScheduler;

impl StoreFlushScheduler for ImmediateScheduler {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        callback();
    }
}

/// Creates a synchronous, reference-stable snapshot store for one card projection.
#[must_use]
pub fn card_snapshot_store<T: Clone + 'static>(initial: T) -> Rc<SnapshotStore<T>> {
    SnapshotStore::new(
        initial,
        StoreFlushMode::Sync,
        Rc::new(ImmediateScheduler),
        None,
        Rc::new(|_| {}) as StoreLogger,
    )
}

/// Staged form over one bound Settings namespace.
pub struct CardForm {
    scope: Rc<dyn ClientSettingsScope<Value>>,
    specs: IndexMap<String, CardFieldSpec>,
    secret_specs: IndexMap<String, CardSecretSpec>,
    staged: RefCell<IndexMap<String, StagedEdit>>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    saving: Cell<bool>,
    failed: Cell<bool>,
    _scope_subscription: ClientSettingsDisposer,
}

impl std::fmt::Debug for CardForm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CardForm")
            .field("specs", &self.specs.keys().collect::<Vec<_>>())
            .field("secrets", &self.secret_specs.keys().collect::<Vec<_>>())
            .field("staged", &self.staged.borrow().len())
            .field("saving", &self.saving.get())
            .field("failed", &self.failed.get())
            .finish_non_exhaustive()
    }
}

impl CardForm {
    /// Creates one form and subscribes its projections to the bound scope.
    #[must_use]
    pub fn new(
        scope: Rc<dyn ClientSettingsScope<Value>>,
        specs: Vec<CardFieldSpec>,
        secrets: Vec<CardSecretSpec>,
    ) -> Rc<Self> {
        let specs = specs
            .into_iter()
            .map(|spec| (spec.field().to_owned(), spec))
            .collect();
        let secret_specs = secrets
            .into_iter()
            .map(|spec| (spec.field.clone(), spec))
            .collect();
        Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let publish = weak.clone();
            let subscription = scope.subscribe(Rc::new(move || {
                if let Some(form) = publish.upgrade() {
                    form.publish();
                }
            }));
            Self {
                scope,
                specs,
                secret_specs,
                staged: RefCell::new(IndexMap::new()),
                listeners: RefCell::new(Vec::new()),
                saving: Cell::new(false),
                failed: Cell::new(false),
                _scope_subscription: subscription,
            }
        })
    }

    /// Adds a projection listener in declaration order.
    pub fn subscribe_projection(&self, listener: Rc<dyn Fn()>) {
        self.listeners.borrow_mut().push(listener);
    }

    /// Current card-level state.
    #[must_use]
    pub fn shell(&self) -> CardShell {
        let plan = self.plan();
        let snapshot = self.scope.snapshot();
        CardShell {
            available: snapshot.status == ClientSettingsStatus::Ready,
            writable: snapshot.writable,
            dirty: !plan.is_empty(),
            invalid: plan
                .iter()
                .any(|write| matches!(write, PlannedWrite::Invalid)),
            saving: self.saving.get(),
            failed: self.failed.get(),
        }
    }

    /// Current state for one declared section or secret field.
    ///
    /// # Panics
    ///
    /// Panics when `field` was never declared by this card.
    #[must_use]
    pub fn field(&self, field: &str) -> CardFieldState {
        let staged = self.staged.borrow().get(field).cloned();
        if self.secret_specs.contains_key(field) {
            return CardFieldState {
                text: staged.map_or_else(String::new, |edit| edit.text),
                overridden: false,
                invalid: false,
            };
        }
        let spec = self.spec(field);
        let Some(staged) = staged else {
            return CardFieldState {
                text: spec.format(&self.section_value(field).unwrap_or(Value::Null)),
                overridden: self.stored(field),
                invalid: false,
            };
        };
        let write = if staged.clear {
            Some(FieldWrite::Clear)
        } else {
            spec.parse(&staged.text)
        };
        CardFieldState {
            text: staged.text,
            overridden: matches!(write, Some(FieldWrite::Set(_))),
            invalid: write.is_none(),
        }
    }

    /// Stages exact draft text.
    pub fn edit(&self, field: impl Into<String>, text: impl Into<String>) {
        self.stage(
            field.into(),
            StagedEdit {
                text: text.into(),
                clear: false,
            },
        );
    }

    /// Stages a clear back to the composition layer.
    ///
    /// # Panics
    ///
    /// Panics when `field` was never declared as a section field.
    pub fn reset_field(&self, field: &str) {
        self.try_reset_field(field)
            .unwrap_or_else(|error| panic!("{error}"));
    }

    /// Stages a checked field clear for browser action adapters.
    ///
    /// # Errors
    ///
    /// Returns the source diagnostic when the card never declared `field`.
    pub fn try_reset_field(&self, field: &str) -> Result<(), String> {
        let Some(spec) = self.specs.get(field) else {
            return Err(format!("plugin card has no field {field}"));
        };
        let text = spec.format(&self.base_value(field).unwrap_or(Value::Null));
        self.stage(field.to_owned(), StagedEdit { text, clear: true });
        Ok(())
    }

    /// Drops every staged edit and prior failure.
    pub fn discard(&self) {
        if self.staged.borrow().is_empty() && !self.failed.get() {
            return;
        }
        self.staged.borrow_mut().clear();
        self.failed.set(false);
        self.publish();
    }

    /// Writes each valid staged edit in staging order and reads back Host authority.
    ///
    /// # Errors
    ///
    /// Returns a scope transport failure. Like the source async method, that exceptional path
    /// leaves the form in its in-flight posture because no settlement was observed.
    pub async fn save(&self) -> Result<(), String> {
        let plan = self.plan();
        if plan.is_empty()
            || self.saving.get()
            || plan
                .iter()
                .any(|write| matches!(write, PlannedWrite::Invalid))
        {
            return Ok(());
        }
        self.saving.set(true);
        self.failed.set(false);
        self.publish();
        let mut landed = true;
        for write in plan {
            let current = match write {
                PlannedWrite::Invalid => unreachable!("invalid plans are refused before saving"),
                PlannedWrite::Set { field, value } => {
                    self.scope.set(field.clone(), value.clone()).await?;
                    self.user_value(&field).as_ref() == Some(&value)
                }
                PlannedWrite::Clear { field } => {
                    self.scope.unset(field.clone()).await?;
                    !self.stored(&field)
                }
                PlannedWrite::Secret { write, value } => write(value).await,
            };
            landed = current && landed;
        }
        if landed {
            self.staged.borrow_mut().clear();
        }
        self.saving.set(false);
        self.failed.set(!landed);
        self.publish();
        Ok(())
    }

    fn plan(&self) -> Vec<PlannedWrite> {
        let mut plan = Vec::new();
        for (field, staged) in self.staged.borrow().iter() {
            if let Some(secret) = self.secret_specs.get(field) {
                let value = trim_ecmascript_whitespace(&staged.text);
                if !value.is_empty() {
                    plan.push(PlannedWrite::Secret {
                        write: secret.write.clone(),
                        value: value.to_owned(),
                    });
                }
                continue;
            }
            let spec = self.spec(field);
            if staged.clear {
                if self.stored(field) {
                    plan.push(PlannedWrite::Clear {
                        field: field.clone(),
                    });
                }
                continue;
            }
            if staged.text == spec.format(&self.section_value(field).unwrap_or(Value::Null)) {
                continue;
            }
            match spec.parse(&staged.text) {
                None => plan.push(PlannedWrite::Invalid),
                Some(FieldWrite::Clear) => plan.push(PlannedWrite::Clear {
                    field: field.clone(),
                }),
                Some(FieldWrite::Set(value)) => plan.push(PlannedWrite::Set {
                    field: field.clone(),
                    value,
                }),
            }
        }
        plan
    }

    fn stage(&self, field: String, edit: StagedEdit) {
        self.staged.borrow_mut().insert(field, edit);
        self.failed.set(false);
        self.publish();
    }

    fn spec(&self, field: &str) -> &CardFieldSpec {
        self.specs
            .get(field)
            .unwrap_or_else(|| panic!("plugin card has no field {field}"))
    }

    fn snapshot(&self) -> Rc<ClientSettingsScopeSnapshot<Value>> {
        self.scope.snapshot()
    }

    fn section_value(&self, field: &str) -> Option<Value> {
        self.snapshot()
            .value
            .as_deref()
            .and_then(Value::as_object)
            .and_then(|value| value.get(field))
            .cloned()
    }

    fn base_value(&self, field: &str) -> Option<Value> {
        self.snapshot()
            .base
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|value| value.get(field))
            .cloned()
    }

    fn user_value(&self, field: &str) -> Option<Value> {
        self.snapshot()
            .user
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|value| value.get(field))
            .cloned()
    }

    fn stored(&self, field: &str) -> bool {
        self.snapshot()
            .user
            .as_ref()
            .and_then(Value::as_object)
            .is_some_and(|user| user.contains_key(field))
    }

    fn publish(&self) {
        for listener in self.listeners.borrow().iter() {
            listener();
        }
    }
}

fn ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            | '\u{2001}'
            | '\u{2002}'
            | '\u{2003}'
            | '\u{2004}'
            | '\u{2005}'
            | '\u{2006}'
            | '\u{2007}'
            | '\u{2008}'
            | '\u{2009}'
            | '\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// JavaScript `String.trim` boundary used by every staged field.
#[must_use]
pub fn trim_ecmascript_whitespace(value: &str) -> &str {
    value.trim_matches(ecmascript_whitespace)
}

fn parse_js_number(value: &str) -> Option<f64> {
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = value.strip_prefix(prefix) {
            if digits.is_empty() {
                return None;
            }
            let mut number = 0.0_f64;
            for character in digits.chars() {
                let digit = character.to_digit(radix)?;
                number = number.mul_add(f64::from(radix), f64::from(digit));
            }
            return Some(number);
        }
    }
    value.parse().ok()
}

fn javascript_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        ryu_js::Buffer::new().format(value).to_owned()
    }
}
