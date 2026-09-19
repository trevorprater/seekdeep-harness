//! Exact marker parsing cases from the source package.

use seekdeep_shell::{ParsedExitStatus, parse_exit_status};

#[test]
fn preserves_clean_bodies_without_a_marker() {
    assert_eq!(
        parse_exit_status("hi\n\n"),
        ParsedExitStatus::Exit {
            body: "hi\n\n".to_owned(),
            exit_code: 0.0,
        }
    );
    assert_eq!(
        parse_exit_status(""),
        ParsedExitStatus::Exit {
            body: String::new(),
            exit_code: 0.0,
        }
    );
}

#[test]
fn strips_only_a_final_newline_prefixed_exit_marker() {
    assert_eq!(
        parse_exit_status("oops\n[exit code: 3]"),
        ParsedExitStatus::Exit {
            body: "oops".to_owned(),
            exit_code: 3.0,
        }
    );
    assert_eq!(
        parse_exit_status("[exit code: 5]"),
        ParsedExitStatus::Exit {
            body: "[exit code: 5]".to_owned(),
            exit_code: 0.0,
        }
    );
}

#[test]
fn signal_marker_takes_its_own_structured_branch() {
    assert_eq!(
        parse_exit_status("gone\n[killed by signal: SIGKILL]"),
        ParsedExitStatus::Signal {
            body: "gone".to_owned(),
            signal: "SIGKILL".to_owned(),
        }
    );
    assert_eq!(
        parse_exit_status("[killed by signal: SIGKILL]"),
        ParsedExitStatus::Exit {
            body: "[killed by signal: SIGKILL]".to_owned(),
            exit_code: 0.0,
        }
    );
}

#[test]
fn leaves_non_pill_markers_in_the_body() {
    assert_eq!(
        parse_exit_status("slow\n[timed out after 100ms]\n[exit code: 143]"),
        ParsedExitStatus::Exit {
            body: "slow\n[timed out after 100ms]".to_owned(),
            exit_code: 143.0,
        }
    );
}
