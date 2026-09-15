//! Same-session goal-round driver over public agent, session, and goal services.

pub mod index;
pub mod invariant;
pub mod prompt;

pub use index::{INJECT, NAME, apply, plugin};
pub use invariant::{NAME as INVARIANT_NAME, register_invariant};
pub use prompt::render_goal_round_prompt;
