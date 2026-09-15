//! Popup substring filtering and command fuzzy ranking.

use std::borrow::Cow;

use crate::SelectOption;

fn js_trim(value: &str) -> &str {
    value.trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
}

/// Filters popup options by case-insensitive label/detail substring.
#[must_use]
pub fn filter_options<'a>(options: &'a [SelectOption], search: &str) -> Cow<'a, [SelectOption]> {
    let query = js_trim(search).to_lowercase();
    if query.is_empty() {
        return Cow::Borrowed(options);
    }
    Cow::Owned(
        options
            .iter()
            .filter(|option| option_matches_query(option, &query))
            .cloned()
            .collect(),
    )
}

/// Returns source-array positions selected by [`filter_options`].
#[must_use]
pub fn filtered_option_indices(options: &[SelectOption], search: &str) -> Vec<usize> {
    let query = js_trim(search).to_lowercase();
    if query.is_empty() {
        return (0..options.len()).collect();
    }
    options
        .iter()
        .enumerate()
        .filter_map(|(index, option)| option_matches_query(option, &query).then_some(index))
        .collect()
}

fn option_matches_query(option: &SelectOption, query: &str) -> bool {
    option.label.to_lowercase().contains(query)
        || option
            .detail
            .as_ref()
            .is_some_and(|detail| detail.to_lowercase().contains(query))
}

fn boundary_bonus(name: &[u16], index: usize) -> i64 {
    if index == 0
        || matches!(name[index - 1], value if value == u16::from(b'-') || value == u16::from(b'_'))
    {
        8
    } else {
        0
    }
}

/// Scores the strongest ordered-subsequence alignment used by command candidates.
#[must_use]
pub fn fuzzy_score(name: &str, query: &str) -> Option<i64> {
    let name = name.encode_utf16().collect::<Vec<_>>();
    let query = query.encode_utf16().collect::<Vec<_>>();
    if query.is_empty() {
        return Some(0);
    }
    if query.len() > name.len() {
        return None;
    }
    let no_match = i64::MIN;
    let mut previous = vec![no_match; name.len()];
    for index in 0..name.len() {
        if name[index] == query[0] {
            previous[index] = 1 + boundary_bonus(&name, index) - i64::try_from(index).ok()?;
        }
    }
    for query_unit in query.iter().skip(1) {
        let mut current = vec![no_match; name.len()];
        let mut best_gapped = no_match;
        for index in 0..name.len() {
            if let Some(gapped_index) = index.checked_sub(2) {
                let prior = previous[gapped_index];
                if prior != no_match {
                    best_gapped = best_gapped.max(prior + i64::try_from(gapped_index).ok()?);
                }
            }
            if name[index] != *query_unit {
                continue;
            }
            let bonus = 1 + boundary_bonus(&name, index);
            let adjacent = index
                .checked_sub(1)
                .map_or(no_match, |prior| previous[prior]);
            if adjacent != no_match {
                current[index] = adjacent + bonus + 4;
            }
            if best_gapped != no_match {
                current[index] =
                    current[index].max(best_gapped + bonus + 1 - i64::try_from(index).ok()?);
            }
        }
        previous = current;
    }
    previous
        .into_iter()
        .max()
        .filter(|score| *score != no_match)
}

/// Case-insensitive fuzzy filtering with prefix/score/source-index ordering.
#[must_use]
pub fn fuzzy_candidates<'a>(
    candidates: &'a [seekdeep_client_ui_input_trigger::InputTriggerCandidate],
    raw_query: &str,
) -> Vec<&'a seekdeep_client_ui_input_trigger::InputTriggerCandidate> {
    let query = raw_query.to_lowercase();
    if query.is_empty() {
        return candidates.iter().collect();
    }
    let mut ranked = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let name = candidate.name.to_lowercase();
            fuzzy_score(&name, &query)
                .map(|score| (candidate, index, name.starts_with(&query), score))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| left.1.cmp(&right.1))
    });
    ranked.into_iter().map(|ranked| ranked.0).collect()
}

/// Recovers the command name from a Host-confirmed executed line.
#[must_use]
pub fn submitted_command_name(line: &str) -> String {
    let trimmed = js_trim(line);
    let token = trimmed
        .split_once(|character: char| character.is_whitespace() || character == '\u{feff}')
        .map_or(trimmed, |(token, _)| token);
    token.chars().skip(1).collect()
}
