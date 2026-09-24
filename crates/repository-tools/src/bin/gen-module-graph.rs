//! Command-line module graph generation and freshness check.

use seekdeep_repository_tools::{
    agent_note_tree::compiled_repository_root,
    module_graph::{MODULE_GRAPH_OUTPUT, generate_module_graph},
};
use std::process::ExitCode;

fn main() -> ExitCode {
    let check = std::env::args()
        .skip(1)
        .any(|argument| argument == "--check");
    let root = compiled_repository_root();
    match generate_module_graph(root) {
        Ok(content) if check => {
            if std::fs::read_to_string(root.join(MODULE_GRAPH_OUTPUT))
                .ok()
                .as_deref()
                == Some(&content)
            {
                println!("gen-module-graph: {MODULE_GRAPH_OUTPUT} is up to date.");
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "gen-module-graph: {MODULE_GRAPH_OUTPUT} is stale. Run `pnpm run gen-module-graph` and commit {MODULE_GRAPH_OUTPUT}."
                );
                ExitCode::FAILURE
            }
        }
        Ok(content) => match std::fs::write(root.join(MODULE_GRAPH_OUTPUT), content) {
            Ok(()) => {
                println!("gen-module-graph: wrote {MODULE_GRAPH_OUTPUT}.");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("gen-module-graph: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("gen-module-graph: {error:#}");
            ExitCode::FAILURE
        }
    }
}
