//! Pure generation-gated candidate-menu reducer.

use std::borrow::Cow;

use crate::{InputTriggerCandidate, TriggerHit};

/// Candidate-source settlement status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuGroupStatus {
    /// Candidate request is unresolved.
    Pending,
    /// Candidate request settled successfully.
    Ready,
}

/// One source group in menu order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuGroup {
    /// Unique source name.
    pub source: String,
    /// Pending/ready status.
    pub status: MenuGroupStatus,
    /// Ready candidates, empty while pending.
    pub items: Vec<InputTriggerCandidate>,
}

/// Highlighted source/index pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuHighlight {
    /// Source group.
    pub source: String,
    /// Candidate index within the group.
    pub index: usize,
}

/// Complete candidate-menu state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuState {
    /// Whether the menu is open.
    pub open: bool,
    /// Current trigger hit.
    pub hit: Option<TriggerHit>,
    /// Monotonic per-hit generation.
    pub generation: u64,
    /// Source groups in roster order.
    pub groups: Vec<MenuGroup>,
    /// Current candidate highlight.
    pub highlight: Option<MenuHighlight>,
}

/// Closed rest state used by store initialization and fresh menu seeding.
pub const MENU_CLOSED: MenuState = MenuState {
    open: false,
    hit: None,
    generation: 0,
    groups: Vec::new(),
    highlight: None,
};

/// Highlight movement direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveDirection {
    /// Forward/down.
    Next,
    /// Backward/up.
    Previous,
}

/// Pure reducer event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEvent {
    /// New/refined hit, or null to close.
    Hit(Option<TriggerHit>),
    /// One source settled; absent items means empty.
    SourceSettled {
        /// Request generation.
        generation: u64,
        /// Source identity.
        source: String,
        /// Candidate result.
        items: Option<Vec<InputTriggerCandidate>>,
    },
    /// One source failed and is silently removed.
    SourceFailed {
        /// Request generation.
        generation: u64,
        /// Source identity.
        source: String,
    },
    /// Cyclic highlight movement.
    Move(MoveDirection),
    /// Explicit close.
    Close,
}

/// Replaces the source roster with pending groups, preserving other state.
#[must_use]
pub fn seed_groups(state: &MenuState, sources: &[String]) -> MenuState {
    MenuState {
        groups: sources
            .iter()
            .map(|source| MenuGroup {
                source: source.clone(),
                status: MenuGroupStatus::Pending,
                items: Vec::new(),
            })
            .collect(),
        highlight: None,
        ..state.clone()
    }
}

fn closed(state: &MenuState) -> Cow<'_, MenuState> {
    if state.open || state.hit.is_some() || !state.groups.is_empty() || state.highlight.is_some() {
        Cow::Owned(MenuState {
            generation: state.generation,
            ..MenuState::default()
        })
    } else {
        Cow::Borrowed(state)
    }
}

fn first_highlight(groups: &[MenuGroup]) -> Option<MenuHighlight> {
    groups
        .iter()
        .find(|group| group.status == MenuGroupStatus::Ready && !group.items.is_empty())
        .map(|group| MenuHighlight {
            source: group.source.clone(),
            index: 0,
        })
}

fn valid_highlight(
    highlight: Option<&MenuHighlight>,
    groups: &[MenuGroup],
) -> Option<MenuHighlight> {
    let highlight = highlight?;
    groups
        .iter()
        .find(|group| group.source == highlight.source)
        .filter(|group| {
            group.status == MenuGroupStatus::Ready && highlight.index < group.items.len()
        })
        .map(|_| highlight.clone())
}

fn positions(groups: &[MenuGroup]) -> Vec<MenuHighlight> {
    groups
        .iter()
        .filter(|group| group.status == MenuGroupStatus::Ready)
        .flat_map(|group| {
            (0..group.items.len()).map(|index| MenuHighlight {
                source: group.source.clone(),
                index,
            })
        })
        .collect()
}

fn all_ready_empty(groups: &[MenuGroup]) -> bool {
    groups
        .iter()
        .all(|group| group.status == MenuGroupStatus::Ready && group.items.is_empty())
}

fn move_highlight(state: &MenuState, direction: MoveDirection) -> Cow<'_, MenuState> {
    if !state.open {
        return Cow::Borrowed(state);
    }
    let positions = positions(&state.groups);
    if positions.is_empty() {
        return Cow::Borrowed(state);
    }
    let current = state
        .highlight
        .as_ref()
        .and_then(|highlight| positions.iter().position(|position| position == highlight));
    let next = match (current, direction) {
        (None, MoveDirection::Next) => 0,
        (None, MoveDirection::Previous) => positions.len() - 1,
        (Some(current), MoveDirection::Next) => (current + 1) % positions.len(),
        (Some(current), MoveDirection::Previous) => {
            (current + positions.len() - 1) % positions.len()
        }
    };
    if state.highlight.as_ref() == positions.get(next) {
        return Cow::Borrowed(state);
    }
    Cow::Owned(MenuState {
        highlight: positions.get(next).cloned(),
        ..state.clone()
    })
}

/// Applies one menu event; borrowed output is the source's same-reference no-op.
#[must_use]
pub fn menu_reduce<'a>(state: &'a MenuState, event: &MenuEvent) -> Cow<'a, MenuState> {
    match event {
        MenuEvent::Hit(None) | MenuEvent::Close => closed(state),
        MenuEvent::Hit(Some(hit)) => Cow::Owned(MenuState {
            open: true,
            hit: Some(hit.clone()),
            generation: state.generation + 1,
            groups: state
                .groups
                .iter()
                .map(|group| MenuGroup {
                    source: group.source.clone(),
                    status: MenuGroupStatus::Pending,
                    items: Vec::new(),
                })
                .collect(),
            highlight: None,
        }),
        MenuEvent::SourceSettled {
            generation,
            source,
            items,
        } => {
            if !state.open || *generation != state.generation {
                return Cow::Borrowed(state);
            }
            let Some(index) = state
                .groups
                .iter()
                .position(|group| group.source == *source)
            else {
                return Cow::Borrowed(state);
            };
            let mut groups = state.groups.clone();
            groups[index] = MenuGroup {
                source: source.clone(),
                status: MenuGroupStatus::Ready,
                items: items.clone().unwrap_or_default(),
            };
            if all_ready_empty(&groups) {
                return closed(state);
            }
            let highlight = valid_highlight(state.highlight.as_ref(), &groups)
                .or_else(|| first_highlight(&groups));
            Cow::Owned(MenuState {
                groups,
                highlight,
                ..state.clone()
            })
        }
        MenuEvent::SourceFailed { generation, source } => {
            if !state.open
                || *generation != state.generation
                || !state.groups.iter().any(|group| group.source == *source)
            {
                return Cow::Borrowed(state);
            }
            let groups = state
                .groups
                .iter()
                .filter(|group| group.source != *source)
                .cloned()
                .collect::<Vec<_>>();
            if groups.is_empty() || all_ready_empty(&groups) {
                return closed(state);
            }
            let highlight = valid_highlight(state.highlight.as_ref(), &groups)
                .or_else(|| first_highlight(&groups));
            Cow::Owned(MenuState {
                groups,
                highlight,
                ..state.clone()
            })
        }
        MenuEvent::Move(direction) => move_highlight(state, *direction),
    }
}

/// Exact-name lookup in one ready source group.
#[must_use]
pub fn exact_match<'a>(
    groups: &'a [MenuGroup],
    source: &str,
    name: &str,
) -> Option<&'a InputTriggerCandidate> {
    groups
        .iter()
        .find(|group| group.source == source && group.status == MenuGroupStatus::Ready)?
        .items
        .iter()
        .find(|candidate| candidate.name == name)
}
