//! Command-line external `NodeNext` declaration consumer verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    node_next_types::{NodeNextTypesReport, render_node_next_types_report, verify_node_next_types},
};

fn main() -> ExitCode {
    match verify_node_next_types(compiled_repository_root()) {
        Ok(report) => {
            let passed = matches!(report, NodeNextTypesReport::Success { .. });
            if passed {
                print!("{}", render_node_next_types_report(&report));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_node_next_types_report(&report));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-node-next-types: {error:#}");
            ExitCode::FAILURE
        }
    }
}
