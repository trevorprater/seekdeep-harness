use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

#[wasm_bindgen(module = "node:fs")]
extern "C" {
    #[wasm_bindgen(js_name = existsSync)]
    fn exists_sync(path: &str) -> bool;

    #[wasm_bindgen(catch, js_name = chmodSync)]
    fn chmod_sync(path: &str, mode: u32) -> Result<(), JsValue>;
}

#[wasm_bindgen(module = "node:path")]
extern "C" {
    fn dirname(path: &str) -> String;

    #[wasm_bindgen(js_name = join)]
    fn join_four(first: &str, second: &str, third: &str, fourth: &str) -> String;
}

#[wasm_bindgen(module = "node:url")]
extern "C" {
    #[wasm_bindgen(catch, js_name = fileURLToPath)]
    fn file_url_to_path(url: &str) -> Result<String, JsValue>;
}

/// Restores executable permission on both source-owned node-pty helper locations.
///
/// # Errors
/// Preserves Node's URL and chmod exceptions, including their original error objects.
#[wasm_bindgen(js_name = ensureNodePtySpawnHelpers)]
pub fn ensure_spawn_helpers(entry_url: &str, platform: &str, arch: &str) -> Result<(), JsValue> {
    let entry = file_url_to_path(entry_url)?;
    let root = dirname(&dirname(&entry));
    let platform_directory = format!("{platform}-{arch}");
    for helper in [
        join_four(&root, "prebuilds", &platform_directory, "spawn-helper"),
        join_four(&root, "build", "Release", "spawn-helper"),
    ] {
        if exists_sync(&helper) {
            chmod_sync(&helper, 0o755)?;
        }
    }
    Ok(())
}
