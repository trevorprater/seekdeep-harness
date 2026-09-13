//! Captured status/streams, trimming, failure text, environment, and inherited runs.

use std::{collections::BTreeMap, ffi::OsString};

use seekdeep_repository_tools::release_process::{ReleaseRunOptions, attempt, capture, run};

#[test]
fn attempt_captures_streams_and_leaves_nonzero_status_to_the_caller() {
    let result = attempt(
        "node",
        &[
            "-e".to_owned(),
            "process.stdout.write('out'); process.stderr.write('err'); process.exit(3)".to_owned(),
        ],
        &ReleaseRunOptions::default(),
    )
    .unwrap();
    assert_eq!(result.status, Some(3));
    assert_eq!(result.stdout, "out");
    assert_eq!(result.stderr, "err");
}

#[test]
fn capture_trims_success_and_formats_failure_with_both_streams() {
    assert_eq!(
        capture(
            "node",
            &["-e".to_owned(), "console.log('  value  ')".to_owned()],
            &ReleaseRunOptions::default(),
        )
        .unwrap(),
        "value"
    );
    let error = capture(
        "node",
        &[
            "-e".to_owned(),
            "process.stdout.write('out'); process.stderr.write('err'); process.exit(4)".to_owned(),
        ],
        &ReleaseRunOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("node -e process.stdout.write"));
    assert!(error.contains("exited with 4:\nout\nerr"));
}

#[test]
fn supplied_environment_replaces_the_child_environment() {
    let options = ReleaseRunOptions {
        cwd: None,
        env: Some(BTreeMap::from([
            (OsString::from("PATH"), std::env::var_os("PATH").unwrap()),
            (OsString::from("ONLY_VALUE"), OsString::from("present")),
        ])),
    };
    assert_eq!(
        capture(
            "node",
            &[
                "-e".to_owned(),
                "process.stdout.write(`${process.env.ONLY_VALUE}:${process.env.HOME ?? 'absent'}`)"
                    .to_owned(),
            ],
            &options,
        )
        .unwrap(),
        "present:absent"
    );
}

#[test]
fn inherited_run_accepts_zero_and_rejects_nonzero() {
    run(
        "node",
        &["-e".to_owned(), "process.exit(0)".to_owned()],
        &ReleaseRunOptions::default(),
    )
    .unwrap();
    let error = run(
        "node",
        &["-e".to_owned(), "process.exit(7)".to_owned()],
        &ReleaseRunOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.ends_with("exited with 7"));
}
