//! Process-level usage and exit parity with the pinned repository demo wrappers.

use std::process::Command;

fn run(binary: &str, arguments: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
    let output = Command::new(binary).args(arguments).output().unwrap();
    (
        output.status.code().unwrap_or(1),
        output.stdout,
        output.stderr,
    )
}

#[test]
fn invalid_code_mode_arguments_match_the_source_oracle_exactly() {
    let (code, stdout, stderr) = run(env!("CARGO_BIN_EXE_seekdeep-demo-code-mode"), &["extra"]);
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(stderr, b"usage: pnpm run demo:code-mode\n");
}

#[test]
fn invalid_cordis_surface_and_arity_match_the_source_oracle_exactly() {
    for arguments in [&["nope"][..], &["web", "extra"][..]] {
        let (code, stdout, stderr) = run(env!("CARGO_BIN_EXE_seekdeep-demo-cordis"), arguments);
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"usage: pnpm run demo:cordis [web|acp]\n");
    }
}
