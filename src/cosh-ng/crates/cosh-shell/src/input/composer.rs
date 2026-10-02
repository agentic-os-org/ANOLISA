//! Command discovery and routing for the Agent-owned composer.

use crate::slash::registry::{active_slash_commands, exact_slash_control_commands};

pub(crate) fn slash_completions(input: &str, row: usize, col: usize) -> Vec<&'static str> {
    let Some((prefix, first_token)) = token_prefix_at_cursor(input, row, col) else {
        return Vec::new();
    };
    if !first_token || !prefix.starts_with('/') || prefix == "/skill" || !is_slash_submission(input)
    {
        return Vec::new();
    }
    // Editing the start of a path or Skill directive must not replace the
    // complete token with a command just because the cursor precedes `/` or `:`.
    if input.split('\n').nth(row).is_some_and(|line| {
        line.chars()
            .skip(col)
            .take_while(|ch| !ch.is_whitespace())
            .any(|ch| matches!(ch, '/' | ':'))
    }) {
        return Vec::new();
    }
    let mut names = Vec::new();
    for name in active_slash_commands().filter(|name| name.starts_with(&prefix)) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

pub(crate) fn is_slash_submission(input: &str) -> bool {
    let token = input.split_whitespace().next().unwrap_or_default();
    // Explicit Agent prompts and skill directives keep their existing route.
    // Keep the menu and exact commands ahead of filesystem paths, as at the
    // shell prompt. Other existing paths remain ordinary Agent text.
    token.strip_prefix('/').is_some_and(|name| {
        !name.contains('/')
            && !name.starts_with("skill:")
            && (token == "/"
                || exact_slash_control_commands().any(|command| command == token)
                || !std::path::Path::new(token).exists())
    })
}

pub(crate) fn token_prefix_at_cursor(
    input: &str,
    cursor_row: usize,
    cursor_col: usize,
) -> Option<(String, bool)> {
    let line = input.split('\n').nth(cursor_row)?;
    let chars = line.chars().collect::<Vec<_>>();
    let cursor_col = cursor_col.min(chars.len());
    let start = chars[..cursor_col]
        .iter()
        .rposition(|ch| ch.is_whitespace())
        .map_or(0, |index| index + 1);
    let prefix = chars[start..cursor_col].iter().collect::<String>();
    let prior_lines_are_empty = input
        .split('\n')
        .take(cursor_row)
        .all(|prior| prior.trim().is_empty());
    let first_token =
        prior_lines_are_empty && chars[..start].iter().collect::<String>().trim().is_empty();
    Some((prefix, first_token))
}
