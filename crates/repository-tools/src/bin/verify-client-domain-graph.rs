//! Command-line compatibility-client domain layering verification.

use std::process::ExitCode;

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    client_domain_graph::{inspect_client_domain_graph, render_client_domain_graph_report},
};

fn main() -> ExitCode {
    match inspect_client_domain_graph(compiled_repository_root()) {
        Ok(violations) => {
            let passed = violations.is_empty();
            if passed {
                print!("{}", render_client_domain_graph_report(&violations));
                ExitCode::SUCCESS
            } else {
                eprint!("{}", render_client_domain_graph_report(&violations));
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("verify-client-domain-graph: {error:#}");
            ExitCode::FAILURE
        }
    }
}
