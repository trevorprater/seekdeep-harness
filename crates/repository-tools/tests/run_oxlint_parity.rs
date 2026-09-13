//! Worker-bound validation plus single- and two-pass process fixtures.

use seekdeep_repository_tools::run_oxlint::{
    OxlintCompletion, OxlintInvocation, resolve_oxlint_invocation, run_oxlint,
};
use tempfile::TempDir;

#[test]
fn ordinary_default_invocation_is_preserved() {
    assert_eq!(
        resolve_oxlint_invocation(&[".".to_owned()], None).unwrap(),
        OxlintInvocation {
            args: vec![".".to_owned()],
            go_max_procs: None,
        }
    );
}

#[test]
fn one_setting_bounds_both_worker_pools() {
    assert_eq!(
        resolve_oxlint_invocation(&[".".to_owned(), "--fix".to_owned()], Some("4")).unwrap(),
        OxlintInvocation {
            args: vec![".".to_owned(), "--fix".to_owned(), "--threads=4".to_owned()],
            go_max_procs: Some("4".to_owned()),
        }
    );
}

#[test]
fn invalid_and_competing_worker_bounds_are_rejected() {
    for value in ["0", "-1", "1.5", "auto", "01", "9007199254740992"] {
        let error = resolve_oxlint_invocation(&[".".to_owned()], Some(value))
            .unwrap_err()
            .to_string();
        assert!(error.contains("SEEKDEEP_OXLINT_THREADS must be a positive integer"));
    }
    let error = resolve_oxlint_invocation(&["--threads=2".to_owned()], Some("4"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("use SEEKDEEP_OXLINT_THREADS instead"));
}

#[test]
fn ordinary_checks_run_one_inherited_pass() {
    let root = fake_oxlint("process.exit(0)");
    assert_eq!(
        run_oxlint(root.path(), &[".".to_owned()], None).unwrap(),
        OxlintCompletion::Exit(0)
    );
    assert_eq!(read_count(&root), 1);
}

#[test]
fn successful_fix_runs_once_and_receives_both_thread_bounds() {
    let root = fake_oxlint(
        "if (!process.argv.includes('--threads=4') || process.env.GOMAXPROCS !== '4') process.exit(9); process.exit(0)",
    );
    assert_eq!(
        run_oxlint(root.path(), &["--fix".to_owned()], Some("4")).unwrap(),
        OxlintCompletion::Exit(0)
    );
    assert_eq!(read_count(&root), 1);
}

#[test]
fn failed_first_fix_discards_that_status_and_runs_exactly_one_retry() {
    let root = fake_oxlint("process.exit(count === 1 ? 7 : 0)");
    assert_eq!(
        run_oxlint(root.path(), &["--fix-dangerously".to_owned()], None).unwrap(),
        OxlintCompletion::Exit(0)
    );
    assert_eq!(read_count(&root), 2);
}

fn fake_oxlint(body: &str) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("node_modules/oxlint/bin/oxlint");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(
        script,
        format!(
            "import fs from 'node:fs';\nconst countFile = new URL('./count.txt', import.meta.url);\nconst count = fs.existsSync(countFile) ? Number(fs.readFileSync(countFile, 'utf8')) + 1 : 1;\nfs.writeFileSync(countFile, String(count));\n{body}\n"
        ),
    )
    .unwrap();
    root
}

fn read_count(root: &TempDir) -> usize {
    std::fs::read_to_string(root.path().join("node_modules/oxlint/bin/count.txt"))
        .unwrap()
        .parse()
        .unwrap()
}
