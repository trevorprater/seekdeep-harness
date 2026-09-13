//! Portable theme registry, preference resolution, overrides, and inspection catalog.

use std::rc::Rc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Built-in durable theme preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    /// Always use the light palette.
    Light,
    /// Always use the dark palette.
    Dark,
    /// Follow the operating-system color-scheme preference.
    #[default]
    System,
}

impl ThemePreference {
    /// Stable settings and protocol spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    /// Narrows an untyped boundary value to a durable built-in preference.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// Registered theme identifier crossing the browser plugin boundary.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThemeId(String);

impl ThemeId {
    /// Creates an identifier with its exact JavaScript spelling.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Live selection, including `system` and registered extension ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThemeSelection(String);

impl ThemeSelection {
    /// Creates a selection with its exact JavaScript spelling.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identity of one source-owned token override layer.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThemeOverrideSource(String);

impl ThemeOverrideSource {
    /// Creates a source identity with its exact JavaScript spelling.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Base palette selected by one concrete theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorScheme {
    /// Light base palette.
    Light,
    /// Dark base palette.
    Dark,
}

impl ColorScheme {
    /// Stable browser spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Parses the closed browser spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// One registered concrete theme.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeDefinition {
    /// Registry identity.
    pub id: ThemeId,
    /// Base palette semantics.
    pub color_scheme: ColorScheme,
    /// Alias-token values over the base palette.
    pub tokens: IndexMap<String, String>,
}

impl ThemeDefinition {
    fn builtin(id: &str, color_scheme: ColorScheme) -> Self {
        Self {
            id: ThemeId::new(id),
            color_scheme,
            tokens: IndexMap::new(),
        }
    }
}

/// Required light and dark values for one override token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeTokenModes {
    /// Value used over the light palette.
    pub light: String,
    /// Value used over the dark palette.
    pub dark: String,
}

/// One source-owned override layer.
pub type ThemeTokenOverrides = IndexMap<String, ThemeTokenModes>;

/// Immutable data published for one registry revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeSnapshot {
    /// Live preference; custom registered IDs remain process-local.
    pub preference: ThemeSelection,
    /// Resolved concrete theme with override layers composed.
    pub active: ThemeDefinition,
    /// Registered themes in registration order.
    pub themes: Vec<ThemeDefinition>,
    /// Monotonic publish counter.
    pub revision: u64,
}

/// One token description exposed to Cordis inspection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeTokenInspection {
    /// Token identity accepted by override layers.
    pub name: String,
    /// Intended visual role.
    pub description: String,
    /// CSS value category.
    pub value_type: String,
    /// Whether both palettes are required.
    pub requires_light_and_dark: bool,
    /// CSS variable, absent for semantic non-variable names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub css_variable: Option<String>,
}

/// Exact-generation registration token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeRegistrationToken(u64);

/// Exact-generation override-layer token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeOverrideToken(u64);

#[derive(Clone, Debug)]
struct RegisteredTheme {
    token: Option<ThemeRegistrationToken>,
    definition: ThemeDefinition,
}

#[derive(Clone, Debug)]
struct OverrideLayer {
    token: ThemeOverrideToken,
    seq: u64,
    tokens: ThemeTokenOverrides,
}

/// Registry boundary error with source-compatible teaching diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ThemeRegistryError {
    /// Unknown setTheme identity.
    #[error("theme {id:?} is not registered")]
    NotRegistered {
        /// Requested id.
        id: String,
    },
    /// Duplicate registration identity.
    #[error("theme {id:?} is already registered")]
    AlreadyRegistered {
        /// Duplicated id.
        id: String,
    },
    /// Reserved system identity.
    #[error("\"system\" is a preference, not a registrable theme id")]
    ReservedSystem,
    /// Override used a bare string.
    #[error(
        "theme override {name:?} from {layer_source:?} is a bare string — pass {{ light: {value:?}, dark: {value:?} }} (repeat the value when it is the same in both palettes); a single value goes illegible when the user switches color scheme"
    )]
    BareOverride {
        /// Layer owner.
        layer_source: String,
        /// Token name.
        name: String,
        /// Rejected single value.
        value: String,
    },
    /// Override omitted one mode or used non-string values.
    #[error(
        "theme override {name:?} from {layer_source:?} must map to a {{ light, dark }} pair of strings — one value per color scheme"
    )]
    InvalidOverridePair {
        /// Layer owner.
        layer_source: String,
        /// Token name.
        name: String,
    },
}

/// Result of a live preference change, including an optional durable write.
#[derive(Clone, Debug)]
pub struct ThemeMutation {
    /// Newly published snapshot.
    pub snapshot: Rc<ThemeSnapshot>,
    /// Built-in value that crosses Host settings, absent for custom themes.
    pub persist: Option<ThemePreference>,
}

/// DOM-free theme registry and preference owner.
#[derive(Clone, Debug)]
pub struct ThemeRegistry {
    themes: Vec<RegisteredTheme>,
    preference: ThemeSelection,
    revision: u64,
    snapshot: Rc<ThemeSnapshot>,
    system_dark: bool,
    overrides: IndexMap<ThemeOverrideSource, OverrideLayer>,
    next_token: u64,
    next_override_seq: u64,
}

impl ThemeRegistry {
    /// Creates the built-in light/dark registry at revision zero.
    #[must_use]
    pub fn new(system_dark: bool) -> Self {
        let themes = vec![
            RegisteredTheme {
                token: None,
                definition: ThemeDefinition::builtin("light", ColorScheme::Light),
            },
            RegisteredTheme {
                token: None,
                definition: ThemeDefinition::builtin("dark", ColorScheme::Dark),
            },
        ];
        let mut registry = Self {
            themes,
            preference: ThemeSelection::new(ThemePreference::System.as_str()),
            revision: 0,
            snapshot: Rc::new(ThemeSnapshot {
                preference: ThemeSelection::new(""),
                active: ThemeDefinition::builtin("light", ColorScheme::Light),
                themes: Vec::new(),
                revision: 0,
            }),
            system_dark,
            overrides: IndexMap::new(),
            next_token: 0,
            next_override_seq: 0,
        };
        registry.snapshot = Rc::new(registry.build_snapshot());
        registry
    }

    /// Stable current snapshot reference until the next publish.
    #[must_use]
    pub fn snapshot(&self) -> Rc<ThemeSnapshot> {
        self.snapshot.clone()
    }

    /// Switches to a registered theme or system preference.
    ///
    /// # Errors
    ///
    /// Rejects an identity that is neither `system` nor a registered theme.
    pub fn set_theme(
        &mut self,
        id: ThemeSelection,
    ) -> Result<Option<ThemeMutation>, ThemeRegistryError> {
        if id.as_str() != "system"
            && !self
                .themes
                .iter()
                .any(|theme| theme.definition.id.as_str() == id.as_str())
        {
            return Err(ThemeRegistryError::NotRegistered {
                id: id.as_str().to_owned(),
            });
        }
        if self.preference == id {
            return Ok(None);
        }
        let persist = ThemePreference::parse(id.as_str());
        self.preference = id;
        Ok(Some(ThemeMutation {
            snapshot: self.publish(),
            persist,
        }))
    }

    /// Adopts a validated Host preference without writing it back.
    pub fn adopt(&mut self, preference: ThemePreference) -> Option<Rc<ThemeSnapshot>> {
        if self.preference.as_str() == preference.as_str() {
            return None;
        }
        self.preference = ThemeSelection::new(preference.as_str());
        Some(self.publish())
    }

    /// Updates OS scheme state and republishes only while following system.
    pub fn set_system_dark(&mut self, dark: bool) -> Option<Rc<ThemeSnapshot>> {
        if self.system_dark == dark {
            return None;
        }
        self.system_dark = dark;
        (self.preference.as_str() == "system").then(|| self.publish())
    }

    /// Registers a theme and publishes its arrival.
    ///
    /// # Errors
    ///
    /// Rejects the reserved `system` id and every duplicate id.
    pub fn register(
        &mut self,
        definition: ThemeDefinition,
    ) -> Result<(ThemeRegistrationToken, Rc<ThemeSnapshot>), ThemeRegistryError> {
        if definition.id.as_str() == "system" {
            return Err(ThemeRegistryError::ReservedSystem);
        }
        if self
            .themes
            .iter()
            .any(|theme| theme.definition.id == definition.id)
        {
            return Err(ThemeRegistryError::AlreadyRegistered {
                id: definition.id.as_str().to_owned(),
            });
        }
        self.next_token = self.next_token.wrapping_add(1);
        let token = ThemeRegistrationToken(self.next_token);
        self.themes.push(RegisteredTheme {
            token: Some(token),
            definition,
        });
        Ok((token, self.publish()))
    }

    /// Disposes exactly the registered generation represented by `token`.
    pub fn dispose_registration(
        &mut self,
        token: ThemeRegistrationToken,
    ) -> Option<Rc<ThemeSnapshot>> {
        let index = self
            .themes
            .iter()
            .position(|theme| theme.token == Some(token))?;
        let removed = self.themes.remove(index);
        if self.preference.as_str() == removed.definition.id.as_str() {
            self.preference = ThemeSelection::new(ThemePreference::System.as_str());
        }
        Some(self.publish())
    }

    /// Installs or replaces one validated override source and restacks it last.
    pub fn override_tokens(
        &mut self,
        source: ThemeOverrideSource,
        tokens: ThemeTokenOverrides,
    ) -> (ThemeOverrideToken, Rc<ThemeSnapshot>) {
        self.next_token = self.next_token.wrapping_add(1);
        self.next_override_seq = self.next_override_seq.wrapping_add(1);
        let token = ThemeOverrideToken(self.next_token);
        self.overrides.insert(
            source,
            OverrideLayer {
                token,
                seq: self.next_override_seq,
                tokens,
            },
        );
        (token, self.publish())
    }

    /// Removes only the current layer generation represented by `token`.
    pub fn dispose_override(
        &mut self,
        source: &ThemeOverrideSource,
        token: ThemeOverrideToken,
    ) -> Option<Rc<ThemeSnapshot>> {
        if self.overrides.get(source)?.token != token {
            return None;
        }
        self.overrides.shift_remove(source);
        Some(self.publish())
    }

    /// Returns a sorted defensive token directory.
    #[must_use]
    pub fn export_inspect_tokens(&self) -> Vec<ThemeTokenInspection> {
        let mut tokens = builtin_inspect_tokens()
            .into_iter()
            .map(|token| (token.name.clone(), token))
            .collect::<IndexMap<_, _>>();
        for theme in &self.themes {
            for name in theme.definition.tokens.keys() {
                tokens
                    .entry(name.clone())
                    .or_insert_with(|| dynamic_token(name));
            }
        }
        for layer in self.overrides.values() {
            for name in layer.tokens.keys() {
                tokens
                    .entry(name.clone())
                    .or_insert_with(|| dynamic_token(name));
            }
        }
        let mut values = tokens.into_values().collect::<Vec<_>>();
        values.sort_by(|left, right| left.name.cmp(&right.name));
        values
    }

    fn publish(&mut self) -> Rc<ThemeSnapshot> {
        self.revision = self.revision.wrapping_add(1);
        self.snapshot = Rc::new(self.build_snapshot());
        self.snapshot.clone()
    }

    fn build_snapshot(&self) -> ThemeSnapshot {
        let resolved_id = if self.preference.as_str() == "system" {
            if self.system_dark { "dark" } else { "light" }
        } else {
            self.preference.as_str()
        };
        let active = self
            .themes
            .iter()
            .find(|theme| theme.definition.id.as_str() == resolved_id)
            .expect("built-ins cannot be removed and active custom ids reset on disposal")
            .definition
            .clone();
        ThemeSnapshot {
            preference: self.preference.clone(),
            active: self.compose_active(active),
            themes: self
                .themes
                .iter()
                .map(|theme| theme.definition.clone())
                .collect(),
            revision: self.revision,
        }
    }

    fn compose_active(&self, mut active: ThemeDefinition) -> ThemeDefinition {
        if self.overrides.is_empty() {
            return active;
        }
        let mut layers = self.overrides.values().collect::<Vec<_>>();
        layers.sort_by_key(|layer| layer.seq);
        for layer in layers {
            for (name, modes) in &layer.tokens {
                active.tokens.insert(
                    name.clone(),
                    match active.color_scheme {
                        ColorScheme::Light => modes.light.clone(),
                        ColorScheme::Dark => modes.dark.clone(),
                    },
                );
            }
        }
        active
    }
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new(false)
    }
}

fn dynamic_token(name: &str) -> ThemeTokenInspection {
    ThemeTokenInspection {
        name: name.to_owned(),
        description: "Theme token registered by the current Client composition.".to_owned(),
        value_type: "CSS value".to_owned(),
        requires_light_and_dark: true,
        css_variable: name.starts_with("--").then(|| name.to_owned()),
    }
}

fn builtin_inspect_tokens() -> Vec<ThemeTokenInspection> {
    [
        ("--dsw-alias-bg-base", "Application base background."),
        (
            "--dsw-alias-bg-layer-1",
            "Primary raised surface background.",
        ),
        (
            "--dsw-alias-bg-layer-2",
            "Secondary nested surface background.",
        ),
        ("--dsw-alias-bg-overlay", "Overlay and popover background."),
        ("--dsw-alias-border-l1", "Primary subtle border."),
        ("--dsw-alias-border-l2", "Secondary stronger border."),
        ("--dsw-alias-brand-primary", "Primary brand accent."),
        ("--dsw-alias-label-primary", "Primary text color."),
        ("--dsw-alias-label-secondary", "Secondary text color."),
        (
            "--dsw-alias-state-error-primary",
            "Primary error state color.",
        ),
        (
            "--dsw-alias-state-success-primary",
            "Primary success state color.",
        ),
        (
            "--dsw-alias-state-warn-primary",
            "Primary warning state color.",
        ),
        (
            "--dsw-specific-sidebar-fill",
            "Sidebar column and title-row background.",
        ),
    ]
    .into_iter()
    .map(|(name, description)| ThemeTokenInspection {
        name: name.to_owned(),
        description: description.to_owned(),
        value_type: "CSS color".to_owned(),
        requires_light_and_dark: true,
        css_variable: Some(name.to_owned()),
    })
    .collect()
}
