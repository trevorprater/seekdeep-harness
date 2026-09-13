//! Host-rendered pre-plugin theme bootstrap.

use regex::Regex;

use crate::ThemePreference;

/// Injects the source-equivalent inline bootstrap immediately after `<body>`.
#[must_use]
pub fn inject_boot_theme(html: &str, preference: ThemePreference) -> String {
    let script = format!(
        "<script>(() => {{
  const preference = {:?}
  const systemDark = preference === 'system'
    && typeof matchMedia !== 'undefined'
    && matchMedia('(prefers-color-scheme: dark)').matches
  const dark = preference === 'dark' || systemDark
  document.documentElement.style.colorScheme = dark ? 'dark' : 'light'
  document.body.toggleAttribute('data-ds-dark-theme', dark)
}})()</script>",
        preference.as_str()
    );
    let body = Regex::new(r"(?i)<body(?:\s[^>]*)?>")
        .ok()
        .and_then(|pattern| pattern.find(html));
    body.map_or_else(
        || format!("{html}{script}"),
        |body| {
            let at = body.end();
            format!("{}{script}{}", &html[..at], &html[at..])
        },
    )
}
