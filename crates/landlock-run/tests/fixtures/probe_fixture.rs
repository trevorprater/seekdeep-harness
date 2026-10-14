//! Immutable native executable used by the probe's classification tests.

fn main() {
    assert_eq!(std::env::args().nth(1).as_deref(), Some("--probe"));
    let executable = std::path::PathBuf::from(std::env::args_os().next().unwrap());
    match executable.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("full") => println!("landlock: fully enforced"),
        Some("partial") => println!("landlock: partially enforced (older ABI)"),
        Some("timeout") => std::thread::sleep(std::time::Duration::from_secs(10)),
        _ => std::process::exit(125),
    }
}
