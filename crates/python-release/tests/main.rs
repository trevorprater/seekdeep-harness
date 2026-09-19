//! One test binary per crate: every top-level test file is a module here.

mod executable_parity;
mod node_runtime_packaging;
mod release_parity;
mod ripgrep_packaging;
mod workflow_contract;

#[path = "common/node_fixture.rs"]
mod node_fixture;
