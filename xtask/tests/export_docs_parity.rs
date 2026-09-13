//! Negative-path coverage for the exported Rust API documentation gate.

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn check(source: &str) -> (bool, String) {
    let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("seekdeep-export-docs-{}-{id}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let source_path = root.join("lib.rs");
    fs::write(
        &source_path,
        format!("//! Fixture crate.\n#![deny(missing_docs)]\n{source}"),
    )
    .unwrap();
    let output_path: PathBuf = root.join("fixture.rlib");
    let output = Command::new("rustc")
        .args(["--crate-type=lib", "--edition=2024"])
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .unwrap();
    fs::remove_dir_all(root).unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn fully_documented_public_api_passes_without_param_or_return_tag_duplication() {
    let (passed, stderr) = check(
        r"
/// Adds one to a count.
pub fn bump(value: u64) -> u64 { value + 1 }

/// Default retry budget.
pub const RETRIES: u64 = 3;

/// Public record.
pub struct Record {
    /// Stored value.
    pub value: u64,
}

/// Public behavior.
pub trait Run {
    /// Runs the behavior.
    fn run(&self);
}
",
    );
    assert!(passed, "{stderr}");
}

#[test]
fn undocumented_public_functions_constants_and_type_forms_fail_closed() {
    let (passed, stderr) = check(
        r"
pub fn bare() {}
pub const LIMIT: u64 = 10;
pub type Count = u64;
pub enum Mode { One }
pub trait Work { fn work(&self); }
",
    );
    assert!(!passed);
    for diagnostic in [
        "missing documentation for a function",
        "missing documentation for a constant",
        "missing documentation for a type alias",
        "missing documentation for an enum",
        "missing documentation for a variant",
        "missing documentation for a trait",
        "missing documentation for a method",
    ] {
        assert!(stderr.contains(diagnostic), "{diagnostic}: {stderr}");
    }
}

#[test]
fn exported_struct_fields_and_inherent_methods_require_their_own_prose() {
    let (passed, stderr) = check(
        r"
/// Public record.
pub struct Record { pub value: u64 }
impl Record { pub fn read(&self) -> u64 { self.value } }
",
    );
    assert!(!passed);
    assert!(
        stderr.contains("missing documentation for a struct field"),
        "{stderr}"
    );
    assert!(
        stderr.contains("missing documentation for a method"),
        "{stderr}"
    );
}

#[test]
fn private_items_constructors_and_trait_implementation_members_are_exempt() {
    let (passed, stderr) = check(
        r"
fn helper() {}
struct Private { field: u64 }

/// Public seam.
pub trait Run {
    /// Runs the seam.
    fn run(&self);
}

/// Public implementation.
pub struct Implementation;
impl Implementation {
    fn private_method(&self) { helper(); let _ = Private { field: 1 }.field; }
}
impl Run for Implementation { fn run(&self) { self.private_method(); } }
",
    );
    assert!(passed, "{stderr}");
}

#[test]
fn public_modules_and_nested_exports_are_checked_recursively() {
    let (passed, stderr) = check(
        r"
pub mod nested { pub fn exposed() {} }
",
    );
    assert!(!passed);
    assert!(
        stderr.contains("missing documentation for a module"),
        "{stderr}"
    );
    assert!(
        stderr.contains("missing documentation for a function"),
        "{stderr}"
    );
}
