//! Source-pinned Cordis browser dictionaries.

use std::{collections::BTreeMap, sync::LazyLock};

use serde::Deserialize;

/// Locale namespace registered by the browser plugin.
pub const CORDIS_LOCALE_NAMESPACE: &str = "cordis";

#[derive(Debug, Deserialize)]
struct LocaleCatalog {
    namespace: String,
    en: BTreeMap<String, String>,
    zh: BTreeMap<String, String>,
}

static CATALOG: LazyLock<LocaleCatalog> = LazyLock::new(|| {
    let catalog: LocaleCatalog = serde_json::from_str(include_str!("../data/locales.json"))
        .expect("generated Cordis locale catalog must be valid");
    assert_eq!(catalog.namespace, CORDIS_LOCALE_NAMESPACE);
    catalog
});

/// Returns the exact source-pinned English dictionary.
#[must_use]
pub fn english_locale() -> &'static BTreeMap<String, String> {
    &CATALOG.en
}

/// Returns the exact source-pinned Simplified Chinese dictionary.
#[must_use]
pub fn chinese_locale() -> &'static BTreeMap<String, String> {
    &CATALOG.zh
}

/// Resolves an exact Cordis message by locale and key.
#[must_use]
pub fn locale_message(locale: &str, key: &str) -> Option<&'static str> {
    let dictionary = match locale {
        "en" => english_locale(),
        "zh" => chinese_locale(),
        _ => return None,
    };
    dictionary.get(key).map(String::as_str)
}
