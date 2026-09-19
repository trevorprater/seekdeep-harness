//! Rust subprocess and Agent-turn helpers for assembled Loader fixtures.

mod agent_turn;
mod process;

pub use agent_turn::{
    FixtureEventObserver, FixtureTurnOptions, FixtureTurnResult, FixtureTurnResultKind,
    run_fixture_turn,
};
pub use process::{
    EXAMPLE_MODE_ENV, ExampleLaunch, ExampleLaunchOptions, ExampleMode,
    LOADER_SMOKE_TEST_TIMEOUT_MS, LoaderSmokeHook, LoaderSmokeOptions, LoaderSmokeResult,
    SEEKDEEP_AGENTS_HOME_ENV, resolve_example_launch, resolve_example_mode, run_loader_smoke,
};
