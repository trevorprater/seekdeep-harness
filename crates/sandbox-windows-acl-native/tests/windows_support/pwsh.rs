//! Source-compatible PowerShell 7 resolution for native Windows tests.

use std::{collections::BTreeMap, process::Command, sync::OnceLock};

use seekdeep_pwsh_local::{PwshPlatform, resolve_pwsh_path};

pub fn pwsh_path() -> Option<&'static str> {
    static RESOLVED: OnceLock<Option<String>> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            let environment = std::env::vars().collect::<BTreeMap<_, _>>();
            let executable = resolve_pwsh_path(None, &environment, PwshPlatform::Windows);
            Command::new(&executable)
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "$true",
                ])
                .output()
                .is_ok_and(|output| output.status.success())
                .then_some(executable)
        })
        .as_deref()
}
