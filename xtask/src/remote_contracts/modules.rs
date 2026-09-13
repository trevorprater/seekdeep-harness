//! Public package values materialized by the compiled Remote constructor.

use std::path::{Component, Path};

use base64::Engine as _;
use serde_json::Value;

pub(super) fn write(
    root: &Path,
    output_root: &Path,
    bindings: &str,
    wasm: &[u8],
    global: &str,
) -> anyhow::Result<()> {
    let model: Value = serde_json::from_slice(&std::fs::read(
        root.join("crates/api-remotes-client/contracts/host-model.json"),
    )?)?;
    let pin = include_str!("../../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or_else(|| anyhow::anyhow!("source pin absent"))?;
    anyhow::ensure!(
        model["sourceCommit"] == pin,
        "Remote module model has a stale source pin"
    );
    let packages = model["face"]["packages"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Remote packages absent"))?;
    for (index, name) in super::ORDER.iter().enumerate() {
        let package = packages
            .iter()
            .find(|package| package["name"] == *name)
            .ok_or_else(|| anyhow::anyhow!("Remote package {name} missing"))?;
        let path = package["root"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Remote package root absent"))?;
        let package_path = Path::new(path);
        anyhow::ensure!(
            package_path.starts_with("packages")
                && package_path.components().count() == 3
                && package_path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "invalid Remote package root: {path}"
        );
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(root.join(path).join("package.json"))?)?;
        anyhow::ensure!(
            manifest["name"] == super::normalize(name),
            "Remote package identity mismatch at {path}"
        );
        anyhow::ensure!(
            manifest["exports"]["./remote"]["types"] == "./lib/typert.remote-client.d.ts"
                && manifest["exports"]["./remote"]["default"] == "./lib/typert.remote-client.js",
            "Remote package must opt into its generated value and declaration: {path}"
        );
        let output = output_root.join(path).join("lib/typert.remote-client.js");
        let entry = entry(bindings, wasm, global, index)?;
        std::fs::create_dir_all(
            output
                .parent()
                .ok_or_else(|| anyhow::anyhow!("Remote output parent absent"))?,
        )?;
        std::fs::write(output, entry)?;
    }
    println!(
        "built {} public Rust/WASM Remote modules",
        super::ORDER.len()
    );
    Ok(())
}

fn entry(bindings: &str, wasm: &[u8], global: &str, index: usize) -> anyhow::Result<String> {
    let bindings = super::super::named_wasm_bindings(bindings, global)?;
    let bytes = base64::engine::general_purpose::STANDARD.encode(wasm);
    Ok(format!(
        "import {{ z as __seekdeepRemoteZod }} from 'zod';\n{bindings}\nconst binary = {bytes:?};\nconst bytes = Uint8Array.from(atob(binary), value => value.charCodeAt(0));\n{global}.initSync({{ module: bytes }});\n{global}.configureApiRemotesZod(__seekdeepRemoteZod);\nexport const TYPERT_REMOTE = {global}.generatedApiRemote({index});\nexport default TYPERT_REMOTE;\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_entry_delegates_construction_and_exports_only_the_contribution() {
        let result = entry("let wasm_bindgen = {};", &[1, 2, 3], "runtime", 2).unwrap();
        assert!(result.contains("var runtime = {};"));
        assert!(result.contains("from 'zod'"));
        assert!(result.contains("runtime.generatedApiRemote(2)"));
        assert!(result.contains("runtime.configureApiRemotesZod(__seekdeepRemoteZod)"));
        assert!(result.contains("export default TYPERT_REMOTE;"));
        assert!(!result.contains("__ModuleLoader__"));
        assert!(!result.contains("schema.parse"));
    }
}
