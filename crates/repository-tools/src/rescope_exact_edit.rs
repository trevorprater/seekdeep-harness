//! Exact-edit state classification for the vendor rescope codemod.

/// One exact edit's current state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactEditState {
    /// Source form present, target absent.
    Pending,
    /// Target form present exactly as expected.
    Applied,
    /// Partial, moved, duplicated, or absent site.
    Invalid,
}

/// Classifies one exact insertion, deletion, or replacement.
#[must_use]
pub fn exact_edit_state(text: &str, find: &str, replace: &str, expected: usize) -> ExactEditState {
    let hits = text.matches(find).count();
    let landed = text.matches(replace).count();
    if replace.contains(find) {
        if landed == expected {
            return ExactEditState::Applied;
        }
        return if landed == 0 && hits == expected {
            ExactEditState::Pending
        } else {
            ExactEditState::Invalid
        };
    }
    if find.contains(replace) {
        if hits == 0 {
            return if landed == expected {
                ExactEditState::Applied
            } else {
                ExactEditState::Invalid
            };
        }
        return if hits == expected {
            ExactEditState::Pending
        } else {
            ExactEditState::Invalid
        };
    }
    if hits == 0 && landed == expected {
        ExactEditState::Applied
    } else if hits == expected && landed == 0 {
        ExactEditState::Pending
    } else {
        ExactEditState::Invalid
    }
}
