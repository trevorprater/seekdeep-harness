//! Tool-pairing balance over a session surface. Compaction changes surface
//! positions, so safe cuts are derived from tool-call/result content in current
//! surface order rather than step markers.

use anyhow::anyhow;
use seekdeep_core::session::{Session, SessionEvent};
use serde_json::Value;

/// Balance of the cut at a sequence's surface position plus an offset.
fn cut_balance(session: &Session, seq: u64, offset: usize) -> anyhow::Result<bool> {
    let seqs = session.surface_nodes();
    let events = session.events();
    let index = seqs
        .iter()
        .position(|node| *node == seq)
        .ok_or_else(|| anyhow!("tool-pairing balance: surface seq {seq} not found"))?;
    Ok(fold_cuts(&seqs, &events)?[index + offset])
}

/// Fold every surface sequence into a per-cut balance prefix.
fn fold_cuts(seqs: &[u64], events: &[SessionEvent]) -> anyhow::Result<Vec<bool>> {
    let mut cuts = Vec::with_capacity(seqs.len() + 1);
    cuts.push(true);
    let mut in_progress = 0i64;
    for seq in seqs {
        let event = event_for_seq(events, *seq)?;
        in_progress += event_delta(event);
        if in_progress < 0 {
            anyhow::bail!(
                "tool-pairing balance: tool/result at surface seq {seq} has no matching tool-call (corrupt surface)"
            );
        }
        cuts.push(in_progress == 0);
    }
    Ok(cuts)
}

fn event_for_seq(events: &[SessionEvent], seq: u64) -> anyhow::Result<&SessionEvent> {
    let index = usize::try_from(seq).map_err(|_| {
        anyhow!("tool-pairing balance: surface seq {seq} has no matching session event (corrupt surface)")
    })?;
    let event = events.get(index).ok_or_else(|| {
        anyhow!("tool-pairing balance: surface seq {seq} has no matching session event (corrupt surface)")
    })?;
    if event.seq != seq {
        anyhow::bail!(
            "tool-pairing balance: surface seq {seq} has no matching session event (corrupt surface)"
        );
    }
    Ok(event)
}

/// Returns how one surface event changes the in-progress tool-call count.
fn event_delta(event: &SessionEvent) -> i64 {
    match event.event_type.as_str() {
        "assistant/message" => {
            let count = event
                .data
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(|content| content.as_array())
                .map_or(0, |blocks| {
                    blocks
                        .iter()
                        .filter(|block| {
                            block.get("type").and_then(Value::as_str) == Some("tool-call")
                        })
                        .count()
                });
            i64::try_from(count).unwrap_or(i64::MAX)
        }
        "tool/result" => -1,
        _ => 0,
    }
}

/// Whether the cut immediately before a current surface sequence is tool-pairing balanced.
///
/// # Errors
///
/// Returns when the seq is absent from the current surface, a surface sequence
/// has no matching log event, or a tool result has no preceding open call.
pub fn tool_pairing_balanced_before(session: &Session, seq: u64) -> anyhow::Result<bool> {
    cut_balance(session, seq, 0)
}

/// Whether the cut immediately after a current surface sequence is tool-pairing balanced.
///
/// # Errors
///
/// Returns when the seq is absent from the current surface, a surface sequence
/// has no matching log event, or a tool result has no preceding open call.
pub fn tool_pairing_balanced_after(session: &Session, seq: u64) -> anyhow::Result<bool> {
    cut_balance(session, seq, 1)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(event_type: &str, seq: u64, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: 0,
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn fold_cuts_balances_tool_pairs() {
        let events = vec![
            event(
                "assistant/message",
                0,
                json!({"message": {"content": [{"type": "tool-call"}, {"type": "tool-call"}]}}),
            ),
            event("tool/result", 1, json!({})),
            event("tool/result", 2, json!({})),
            event("user/message", 3, json!({})),
        ];
        assert_eq!(
            fold_cuts(&[0, 1, 2, 3], &events).expect("fold"),
            vec![true, false, false, true, true]
        );
    }

    #[test]
    fn fold_cuts_rejects_unmatched_tool_result() {
        let events = vec![event("tool/result", 0, json!({}))];
        assert!(fold_cuts(&[0], &events).is_err());
    }
}
