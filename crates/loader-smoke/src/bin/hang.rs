//! Deadline subprocess fixture for the Loader-smoke harness.

use std::io::Write as _;

fn main() {
    println!("fixture hanging");
    std::io::stdout().flush().expect("flush fixture marker");
    loop {
        std::thread::park();
    }
}
