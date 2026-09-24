//! Root npm commands must reject errors in both native and browser Rust code.

use std::{
    path::Path,
    process::{Command, Output},
};

struct Fixture {
    directory: tempfile::TempDir,
}

const HOST_WASM_PACKAGES: &[&str] = &[
    "seekdeep-code-runtime-node",
    "seekdeep-landlock-entry-wasm",
    "seekdeep-docs-site-runtime",
    "seekdeep-subprocess-postinstall",
];

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            directory: tempfile::tempdir().unwrap(),
        };
        for directory in ["bin", "crates/widget/src", "packages/client/widget"] {
            std::fs::create_dir_all(fixture.root().join(directory)).unwrap();
        }
        fixture.write(
            "Cargo.toml",
            "[workspace]\nmembers=[\"crates/*\"]\nresolver=\"3\"\n",
        );
        for package in HOST_WASM_PACKAGES {
            std::fs::create_dir_all(fixture.root().join(format!("crates/{package}/src"))).unwrap();
            fixture.write(
                &format!("crates/{package}/Cargo.toml"),
                &format!("[package]\nname=\"{package}\"\nversion=\"0.0.0\"\nedition=\"2024\"\n"),
            );
            fixture.write(
                &format!("crates/{package}/src/lib.rs"),
                "pub fn valid() -> u32 { 1 }\n",
            );
        }
        fixture.write(
            "crates/widget/Cargo.toml",
            "[package]\nname=\"validation-widget\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[features]\nvalidation=[]\n",
        );
        fixture.write(
            "packages/client/widget/package.json",
            r#"{"name":"@seekdeep-ai/validation-widget","seekdeep":{"client":{"platform":"web"}},"scripts":{"bundle":"cargo xtask wasm-package --package validation-widget --artifact validation_widget --module-id @seekdeep-ai/validation-widget --out-dir packages/client/widget/lib"}}"#,
        );
        fixture.write("package.json", include_str!("../../package.json"));
        fixture.write(
            "rust-toolchain.toml",
            include_str!("../../rust-toolchain.toml"),
        );
        // Cargo passes the subcommand name to external commands. Compiler work uses real
        // Cargo and xtask; the fixture omits Host asset packaging and its Node dependencies.
        fixture.write(
            "cargo-xtask.rs",
            r#"fn main() {
    let args = std::env::args_os().skip(2).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "host-assets") { return; }
    let status = std::process::Command::new(std::env::var_os("SEEKDEEP_TEST_XTASK").unwrap())
        .args(args).status().unwrap();
    std::process::exit(status.code().unwrap_or(1));
}
"#,
        );
        assert_success(&fixture.run(
            "rustc",
            &[
                "--edition=2024",
                "--crate-name",
                "cargo_xtask_fixture",
                "cargo-xtask.rs",
                "-o",
                &format!("bin/cargo-xtask{}", std::env::consts::EXE_SUFFIX),
            ],
        ));
        fixture.source("pub fn valid() -> u32 { 1 }\n");
        fixture
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn write(&self, path: &str, text: &str) {
        std::fs::write(self.root().join(path), text).unwrap();
    }

    fn source(&self, text: &str) {
        self.write("crates/widget/src/lib.rs", text);
    }

    fn run(&self, program: &str, arguments: &[&str]) -> Output {
        let path = std::env::join_paths(std::iter::once(self.root().join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        Command::new(program)
            .args(arguments)
            .current_dir(self.root())
            .env("PATH", path)
            .env("SEEKDEEP_TEST_XTASK", env!("CARGO_BIN_EXE_xtask"))
            .env("CARGO_TARGET_DIR", self.root().join("target"))
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap()
    }

    fn npm(&self, script: &str) -> Output {
        if cfg!(windows) {
            self.run("cmd", &["/C", "npm", "run", script])
        } else {
            self.run("npm", &["run", script])
        }
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "{}", text(output));
}

fn assert_failure(output: &Output, diagnostic: &str) {
    assert!(!output.status.success(), "{}", text(output));
    assert!(text(output).contains(diagnostic), "{}", text(output));
}

#[test]
fn root_typecheck_checks_native_tests_features_and_browser_code() {
    let fixture = Fixture::new();
    assert_success(&fixture.npm("typecheck"));

    fixture.source("#[cfg(all(test, feature = \"validation\"))]\nfn broken() -> u32 { \"native type error\" }\n");
    assert_failure(&fixture.npm("typecheck"), "mismatched types");

    fixture.source("#[cfg(all(target_arch = \"wasm32\", feature = \"validation\"))]\npub fn broken() -> u32 { \"browser type error\" }\n");
    assert_success(&fixture.run(
        "cargo",
        &["check", "--workspace", "--all-targets", "--all-features"],
    ));
    assert_failure(&fixture.npm("typecheck"), "mismatched types");

    fixture.source("pub fn valid() -> u32 { 1 }\n");
    assert_success(&fixture.npm("typecheck:contracts-ready"));

    for package in HOST_WASM_PACKAGES {
        let source = format!("crates/{package}/src/lib.rs");
        fixture.write(&source, "#[cfg(target_arch = \"wasm32\")]\npub fn broken() -> u32 { \"Host WASM type error\" }\n");
        assert_success(&fixture.run(
            "cargo",
            &["check", "--workspace", "--all-targets", "--all-features"],
        ));
        assert_failure(&fixture.npm("typecheck"), "mismatched types");
        fixture.write(&source, "pub fn valid() -> u32 { 1 }\n");
    }
}

#[test]
fn root_lint_rejects_native_and_browser_clippy_warnings() {
    let fixture = Fixture::new();
    assert_success(&fixture.run("cargo", &["xtask", "check-client", "--clippy"]));

    fixture.source("#[cfg(not(target_arch = \"wasm32\"))]\npub fn lint_error(value: u32) -> bool { value == value }\n");
    assert_failure(
        &fixture.npm("lint"),
        "equal expressions as operands to `==`",
    );

    fixture.source("#[cfg(target_arch = \"wasm32\")]\npub fn lint_error(value: u32) -> bool { value == value }\n");
    assert_success(&fixture.run(
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    ));
    assert_failure(
        &fixture.npm("lint"),
        "equal expressions as operands to `==`",
    );
    assert_failure(
        &fixture.npm("lint:contracts-ready"),
        "equal expressions as operands to `==`",
    );

    fixture.source("pub fn valid() -> u32 { 1 }\n");
    for package in HOST_WASM_PACKAGES {
        let source = format!("crates/{package}/src/lib.rs");
        fixture.write(&source, "#[cfg(target_arch = \"wasm32\")]\npub fn lint_error(value: u32) -> bool { value == value }\n");
        assert_failure(
            &fixture.npm("lint"),
            "equal expressions as operands to `==`",
        );
        fixture.write(&source, "pub fn valid() -> u32 { 1 }\n");
    }
}

#[test]
fn client_check_rejects_missing_or_invalid_build_recipes() {
    let fixture = Fixture::new();
    let manifest = "packages/client/widget/package.json";
    fixture.write(
        manifest,
        r#"{"name":"@seekdeep-ai/validation-widget","seekdeep":{"client":{"platform":"web"}}}"#,
    );
    assert_failure(
        &fixture.run("cargo", &["xtask", "check-client"]),
        "has no Rust bundle command",
    );
    std::fs::remove_file(fixture.root().join(manifest)).unwrap();
    assert_failure(
        &fixture.run("cargo", &["xtask", "check-client"]),
        "no Rust Client build recipes found",
    );
}

#[test]
fn client_clippy_fixes_browser_code_in_a_dirty_checkout() {
    let fixture = Fixture::new();
    assert_success(&fixture.run("git", &["init", "--quiet"]));
    fixture.source("#[cfg(target_arch = \"wasm32\")]\npub fn simplify(value: bool) -> bool { value == true }\n");
    assert_failure(
        &fixture.run("cargo", &["xtask", "check-client", "--clippy"]),
        "equality checks against true are unnecessary",
    );
    assert_success(&fixture.run("cargo", &["xtask", "check-client", "--clippy", "--fix"]));
    assert_success(&fixture.run("cargo", &["xtask", "check-client", "--clippy"]));
    assert!(
        !std::fs::read_to_string(fixture.root().join("crates/widget/src/lib.rs"))
            .unwrap()
            .contains("== true")
    );
}
