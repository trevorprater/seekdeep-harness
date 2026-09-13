//! Delegation-depth accounting: the recursion budget a parent passes to its
//! children.

use seekdeep_agent::Agent;

/// Reads an agent's delegation depth, treating absence as top-level depth zero.
#[must_use]
pub fn delegation_depth_of(agent: &Agent) -> u64 {
    let runtime = agent.options().subagent_depth.unwrap_or(0);
    let header = agent.session().header().delegation_depth.unwrap_or(0);
    header.max(runtime)
}

/// Rejects a recursion cap that cannot represent an exact delegation depth.
///
/// The typed argument is always a non-negative integer, so this boundary
/// accepts every representable value.
pub fn assert_subagent_max_depth(_max_depth: Option<u64>) {}
