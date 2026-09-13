//! Restrictive outcome merging across every hook that matched one point.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::{HookDecision, HookOutput};

/// The single decision a hook point resolves to after merging all matched hooks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergedDecision {
    /// No hook expressed a permission decision.
    None,
    /// Permit.
    Allow,
    /// Request confirmation.
    Ask,
    /// Forbid.
    Deny,
}

/// The folded outcome of every hook that matched one point.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedHookOutcome {
    /// The most-restrictive permission decision across all hooks.
    pub decision: MergedDecision,
    /// Joined (blank-line separated) reasons from every blocking or denying hook.
    pub reason: Option<String>,
    /// True when any hook asked to halt.
    pub stop: bool,
    /// The first halting hook's stopReason, when one halted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Every hook's additionalContext, in hook order.
    pub additional_context: Vec<String>,
    /// Every hook's systemMessage, in hook order.
    pub system_messages: Vec<String>,
}

/// Rank one hook's decision for the deny > ask > allow precedence (higher is stricter).
fn rank(decision: Option<HookDecision>) -> u8 {
    match decision {
        Some(HookDecision::Deny | HookDecision::Block) => 3,
        Some(HookDecision::Ask) => 2,
        Some(HookDecision::Approve | HookDecision::Allow) => 1,
        None => 0,
    }
}

/// Collapse a ranked decision back to the merged enum.
fn decision_for_rank(max_rank: u8) -> MergedDecision {
    match max_rank {
        3 => MergedDecision::Deny,
        2 => MergedDecision::Ask,
        1 => MergedDecision::Allow,
        _ => MergedDecision::None,
    }
}

/// Folds outputs (every hook that matched a point, in hook order) into one most-restrictive outcome.
#[must_use]
pub fn merge_hook_outputs(outputs: &[HookOutput]) -> MergedHookOutcome {
    let mut max_rank = 0_u8;
    // Reasons are kept per rank so only objections explaining the winning decision surface.
    let mut reasons_by_rank: HashMap<u8, Vec<String>> = HashMap::new();
    let mut stop = false;
    let mut stop_reason: Option<String> = None;
    let mut additional_context: Vec<String> = Vec::new();
    let mut system_messages: Vec<String> = Vec::new();

    for out in outputs {
        let r = rank(out.decision);
        if r > max_rank {
            max_rank = r;
        }
        if (r == 3 || r == 2)
            && let Some(reason) = &out.reason
            && !reason.is_empty()
        {
            reasons_by_rank.entry(r).or_default().push(reason.clone());
        }
        if out.continue_ == Some(false) && !stop {
            stop = true;
            if let Some(reason) = &out.stop_reason {
                stop_reason = Some(reason.clone());
            }
        }
        if let Some(context) = &out.additional_context
            && !context.is_empty()
        {
            additional_context.push(context.clone());
        }
        if let Some(message) = &out.system_message
            && !message.is_empty()
        {
            system_messages.push(message.clone());
        }
    }

    let reasons = reasons_by_rank.get(&max_rank).cloned().unwrap_or_default();
    MergedHookOutcome {
        decision: decision_for_rank(max_rank),
        reason: (!reasons.is_empty()).then(|| reasons.join("\n\n")),
        stop,
        stop_reason,
        additional_context,
        system_messages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(decision: Option<HookDecision>) -> HookOutput {
        HookOutput {
            exit_code: Some(0),
            stderr: String::new(),
            stdout: String::new(),
            continue_: None,
            stop_reason: None,
            decision,
            reason: None,
            hook_event_name: None,
            additional_context: None,
            system_message: None,
            updated_input: None,
        }
    }

    #[test]
    fn empty_list_yields_a_neutral_outcome() {
        let merged = merge_hook_outputs(&[]);
        assert_eq!(merged.decision, MergedDecision::None);
        assert!(!merged.stop);
        assert!(merged.additional_context.is_empty());
        assert!(merged.system_messages.is_empty());
    }

    #[test]
    fn a_single_allow_yields_allow() {
        assert_eq!(
            merge_hook_outputs(&[out(Some(HookDecision::Allow))]).decision,
            MergedDecision::Allow
        );
        assert_eq!(
            merge_hook_outputs(&[out(Some(HookDecision::Approve))]).decision,
            MergedDecision::Allow
        );
    }

    #[test]
    fn deny_beats_ask_beats_allow_regardless_of_order() {
        assert_eq!(
            merge_hook_outputs(&[out(Some(HookDecision::Allow)), out(Some(HookDecision::Ask)),])
                .decision,
            MergedDecision::Ask
        );
        assert_eq!(
            merge_hook_outputs(&[out(Some(HookDecision::Ask)), out(Some(HookDecision::Deny))])
                .decision,
            MergedDecision::Deny
        );
        assert_eq!(
            merge_hook_outputs(&[
                out(Some(HookDecision::Deny)),
                out(Some(HookDecision::Allow))
            ])
            .decision,
            MergedDecision::Deny
        );
        // block folds to deny
        assert_eq!(
            merge_hook_outputs(&[
                out(Some(HookDecision::Allow)),
                out(Some(HookDecision::Block))
            ])
            .decision,
            MergedDecision::Deny
        );
    }

    #[test]
    fn no_decision_anywhere_yields_none() {
        assert_eq!(
            merge_hook_outputs(&[out(None), out(None)]).decision,
            MergedDecision::None
        );
    }

    fn with_reason(decision: HookDecision, reason: &str) -> HookOutput {
        let mut output = out(Some(decision));
        output.reason = Some(reason.to_owned());
        output
    }

    #[test]
    fn joins_block_deny_reasons_only_from_blocking_hooks() {
        let merged = merge_hook_outputs(&[
            with_reason(HookDecision::Deny, "first objection"),
            with_reason(HookDecision::Allow, "this allow reason is NOT collected"),
            with_reason(HookDecision::Block, "second objection"),
        ]);
        assert_eq!(
            merged.reason.as_deref(),
            Some("first objection\n\nsecond objection")
        );
    }

    #[test]
    fn no_reason_when_nothing_blocked() {
        assert_eq!(
            merge_hook_outputs(&[out(Some(HookDecision::Allow))]).reason,
            None
        );
    }

    #[test]
    fn surfaces_the_reason_of_the_winning_decision() {
        let merged = merge_hook_outputs(&[
            with_reason(HookDecision::Allow, "allow reason - not surfaced"),
            with_reason(HookDecision::Ask, "needs approval"),
        ]);
        assert_eq!(merged.decision, MergedDecision::Ask);
        assert_eq!(merged.reason.as_deref(), Some("needs approval"));
    }

    #[test]
    fn when_deny_wins_ask_reasons_are_dropped() {
        let merged = merge_hook_outputs(&[
            with_reason(
                HookDecision::Ask,
                "ask reason - not surfaced once deny wins",
            ),
            with_reason(HookDecision::Deny, "the real objection"),
        ]);
        assert_eq!(merged.decision, MergedDecision::Deny);
        assert_eq!(merged.reason.as_deref(), Some("the real objection"));
    }

    #[test]
    fn stop_is_sticky_on_the_first_continue_false() {
        let mut halt = out(None);
        halt.continue_ = Some(false);
        halt.stop_reason = Some("halt now".to_owned());
        let mut second_halt = out(None);
        second_halt.continue_ = Some(false);
        second_halt.stop_reason = Some("second halt - ignored".to_owned());
        let mut kept_going = out(None);
        kept_going.continue_ = Some(true);

        let merged = merge_hook_outputs(&[kept_going, halt, second_halt]);
        assert!(merged.stop);
        assert_eq!(merged.stop_reason.as_deref(), Some("halt now"));
    }

    #[test]
    fn no_stop_when_every_hook_continues() {
        let mut kept_going = out(None);
        kept_going.continue_ = Some(true);
        let merged = merge_hook_outputs(&[kept_going, out(None)]);
        assert!(!merged.stop);
        assert_eq!(merged.stop_reason, None);
    }

    #[test]
    fn a_continue_false_with_no_stop_reason_stops_without_a_reason() {
        let mut halt = out(None);
        halt.continue_ = Some(false);
        let merged = merge_hook_outputs(&[halt]);
        assert!(merged.stop);
        assert_eq!(merged.stop_reason, None);
    }

    #[test]
    fn collects_context_and_messages_in_order_skipping_empties() {
        let mut a = out(None);
        a.additional_context = Some("ctx-A".to_owned());
        a.system_message = Some("warn-A".to_owned());
        let mut empty = out(None);
        empty.additional_context = Some(String::new());
        empty.system_message = Some(String::new());
        let mut b = out(None);
        b.additional_context = Some("ctx-B".to_owned());
        let mut warn_b = out(None);
        warn_b.system_message = Some("warn-B".to_owned());

        let merged = merge_hook_outputs(&[a, empty, b, warn_b]);
        assert_eq!(
            merged.additional_context,
            vec!["ctx-A".to_owned(), "ctx-B".to_owned()]
        );
        assert_eq!(
            merged.system_messages,
            vec!["warn-A".to_owned(), "warn-B".to_owned()]
        );
    }
}
