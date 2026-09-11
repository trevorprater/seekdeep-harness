//! Real Rust/WASM Client watch builds, dependency edits, and process shutdown.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};

struct WatchProcess {
    child: Child,
    log: PathBuf,
}

impl WatchProcess {
    fn start(root: &Path, args: &[&str], log_name: &str) -> Self {
        let log = root.join(log_name);
        let file = std::fs::File::create(&log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .arg("dev-web")
            .args(args)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_NET_OFFLINE", "true")
            .spawn()
            .unwrap();
        Self { child, log }
    }

    fn text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn until(&mut self, predicate: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(180);
        while !predicate(&self.text()) {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "watcher stopped early: {}",
                self.text()
            );
            assert!(
                Instant::now() < deadline,
                "watcher timed out: {}",
                self.text()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop(&mut self) {
        kill(
            Pid::from_raw(i32::try_from(self.child.id()).unwrap()),
            Signal::SIGTERM,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(143), "{}", self.text());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "watcher did not tear down: {}",
                self.text()
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for WatchProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = kill(
                Pid::from_raw(i32::try_from(self.child.id()).unwrap()),
                Signal::SIGTERM,
            );
            let _ = self.child.wait();
        }
    }
}

fn fixture(root: &Path) {
    for path in [
        "crates/widget/src",
        "crates/shared/src",
        "crates/api-remotes-client/contracts",
        "packages/client/widget/src",
    ] {
        std::fs::create_dir_all(root.join(path)).unwrap();
    }
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[\"crates/widget\",\"crates/shared\"]\nresolver=\"3\"\n[profile.release]\nopt-level=0\nlto=false\ndebug=false\nstrip=\"debuginfo\"\n").unwrap();
    std::fs::write(
        root.join("rust-toolchain.toml"),
        include_str!("../../rust-toolchain.toml"),
    )
    .unwrap();
    let source_commit = include_str!("../../SOURCE_SNAPSHOT")
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .unwrap();
    std::fs::write(
        root.join("crates/api-remotes-client/contracts/client-declarations.json"),
        serde_json::json!({"formatVersion":1,"sourceCommit":source_commit,"modules":[],"packages":[]}).to_string(),
    ).unwrap();
    std::fs::write(root.join("crates/widget/Cargo.toml"), "[package]\nname=\"seekdeep-client-watch-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[lib]\ncrate-type=[\"cdylib\"]\n[dependencies]\nwasm-bindgen=\"=0.2.127\"\nseekdeep-client-watch-shared={path=\"../shared\"}\n").unwrap();
    std::fs::write(
        root.join("crates/shared/Cargo.toml"),
        "[package]\nname=\"seekdeep-client-watch-shared\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("crates/shared/src/lib.rs"),
        "pub const VERSION: &str = \"watch-v1\";\n",
    )
    .unwrap();
    std::fs::write(root.join("crates/widget/src/lib.rs"), "use wasm_bindgen::prelude::*;\n#[wasm_bindgen]\npub fn version() -> String { format!(\"{}|{}\", seekdeep_client_watch_shared::VERSION, include_str!(\"../../../packages/client/widget/src/Fixture.module.css\")) }\n").unwrap();
    std::fs::write(
        root.join("packages/client/widget/src/Fixture.module.css"),
        ".root { color: red; }\n",
    )
    .unwrap();
    std::fs::write(root.join("packages/client/widget/package.json"), r#"{"name":"@seekdeep-ai/seekdeep-client-watch-fixture","seekdeep":{"client":{"platform":"web"},"bundle":{}},"scripts":{"bundle":"cargo xtask wasm-package --package seekdeep-client-watch-fixture --artifact seekdeep_client_watch_fixture --module-id @seekdeep-ai/seekdeep-client-watch-fixture --out-dir packages/client/widget/lib"}}"#).unwrap();
}

fn consume(root: &Path) -> String {
    let script = r"const fs=require('node:fs'),vm=require('node:vm'); let plugin; const context={TextDecoder,TextEncoder,Uint8Array,WebAssembly,atob,console,window:{__ModuleLoader__:{load({factory}){plugin=factory(()=>{throw new Error('unexpected external')})}}}}; vm.runInNewContext(fs.readFileSync(process.argv[1],'utf8'),context); process.stdout.write(plugin.version());";
    let output = Command::new("node")
        .arg("-e")
        .arg(script)
        .arg(root.join("packages/client/widget/lib/client.js"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn verify_source_maps(root: &Path) {
    let library = root.join("packages/client/widget/lib");
    for file in ["client.js", "client.web.js"] {
        let script = std::fs::read_to_string(library.join(file)).unwrap();
        assert!(script.ends_with("//# sourceMappingURL=client.js.map\n"));
        let map: serde_json::Value =
            serde_json::from_slice(&std::fs::read(library.join(format!("{file}.map"))).unwrap())
                .unwrap();
        assert_eq!(map["file"], "client.js");
        assert!(!map["mappings"].as_str().unwrap().is_empty());
        assert!(
            map["sourcesContent"][0]
                .as_str()
                .unwrap()
                .contains("function")
        );
    }
    let map: serde_json::Value =
        serde_json::from_slice(&std::fs::read(library.join("client_bg.wasm.map")).unwrap())
            .unwrap();
    assert_eq!(map["file"], "client_bg.wasm");
    assert!(!map["mappings"].as_str().unwrap().is_empty());
    assert!(
        map["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source == "../../../crates/widget/src/lib.rs")
    );
    assert!(
        map["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source
                .as_str()
                .is_some_and(|source| source.contains("pub fn version()")))
    );
}

#[test]
fn polling_rebuilds_shared_rust_and_css_then_native_watch_recovers_from_compile_errors() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fixture(&root);
    let mut watch = WatchProcess::start(&root, &["--poll=50"], "poll.log");
    watch.until(|log| {
        log.contains("dev-web: watching 1 seekdeep.client plugin packages (polling 50ms)")
    });
    assert!(consume(&root).starts_with("watch-v1|.root { color: red; }"));
    verify_source_maps(&root);
    let count = watch.text().matches("Rust/WASM classic bundle at").count();
    std::fs::write(
        root.join("crates/shared/src/lib.rs"),
        "pub const VERSION: &str = \"watch-v2\";\n",
    )
    .unwrap();
    watch.until(|log| log.matches("Rust/WASM classic bundle at").count() > count);
    assert!(consume(&root).starts_with("watch-v2|"));
    let count = watch.text().matches("Rust/WASM classic bundle at").count();
    std::fs::write(
        root.join("packages/client/widget/src/Fixture.module.css"),
        ".root { color: blue; }\n",
    )
    .unwrap();
    watch.until(|log| log.matches("Rust/WASM classic bundle at").count() > count);
    assert!(consume(&root).contains("color: blue"));
    watch.stop();

    std::fs::write(root.join("crates/shared/src/lib.rs"), "this is not Rust\n").unwrap();
    let mut native = WatchProcess::start(&root, &[], "native.log");
    native.until(|log| log.contains("waiting for changes"));
    assert!(!native.text().contains("dev-web: watching"));
    assert!(consume(&root).starts_with("watch-v2|"));
    std::fs::write(
        root.join("crates/shared/src/lib.rs"),
        "pub const VERSION: &str = \"watch-v3\";\n",
    )
    .unwrap();
    native.until(|log| log.contains("dev-web: watching 1 seekdeep.client plugin packages:\n"));
    assert!(consume(&root).starts_with("watch-v3|"));
    let count = native.text().matches("Rust/WASM classic bundle at").count();
    std::fs::write(
        root.join("crates/shared/src/lib.rs"),
        "pub const VERSION: &str = \"watch-v4\";\n",
    )
    .unwrap();
    native.until(|log| log.matches("Rust/WASM classic bundle at").count() > count);
    assert!(consume(&root).starts_with("watch-v4|"));
    let failures = native.text().matches("waiting for changes").count();
    std::fs::write(
        root.join("crates/shared/src/lib.rs"),
        "this is not Rust again\n",
    )
    .unwrap();
    native.until(|log| log.matches("waiting for changes").count() > failures);
    assert!(consume(&root).starts_with("watch-v4|"));
    let count = native.text().matches("Rust/WASM classic bundle at").count();
    std::fs::write(
        root.join("crates/shared/src/lib.rs"),
        "pub const VERSION: &str = \"watch-v5\";\n",
    )
    .unwrap();
    native.until(|log| log.matches("Rust/WASM classic bundle at").count() > count);
    assert!(consume(&root).starts_with("watch-v5|"));
    verify_source_maps(&root);
    native.stop();
    println!(
        "actual Rust/WASM Client builds passed: initial readiness, shared Rust edit, CSS edit, binding and WASM source maps, native change/rebuild/error/recovery, failure retains prior artifact, SIGTERM143 after teardown"
    );
}
