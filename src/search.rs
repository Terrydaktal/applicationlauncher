use fuzzy_rank::metadata::{
    MatchedFieldHighlight, MetadataCandidate, MetadataQuery, SearchField, dedup_push_search_field,
};
use fuzzy_rank::ranking::SearchRank;

use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::models::{AppInfo, WindowInfo};
use crate::*;

pub(crate) fn dedup_search_values(values: Vec<(u8, String)>) -> Vec<(u8, String)> {
    let mut deduped: Vec<(u8, String)> = Vec::new();
    for (priority, value) in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if deduped.iter().any(|(existing_priority, existing_value)| {
            *existing_priority == priority && existing_value.eq_ignore_ascii_case(trimmed)
        }) {
            continue;
        }
        deduped.push((priority, trimmed.to_string()));
    }
    deduped
}

pub(crate) fn app_search_values(app: &AppInfo) -> Vec<(u8, String)> {
    let cleaned_exec = clean_exec_cmd(&app.exec);
    let exec_basename = command_basename(&app.exec);
    let desktop_stem = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string());

    let mut owned_values = Vec::new();
    owned_values.push((0, app.name.clone()));
    if let Some(value) = exec_basename {
        owned_values.push((1, value));
    }
    if let Some(value) = desktop_stem {
        owned_values.push((2, value));
    }
    if let Some(value) = app.comment.clone() {
        owned_values.push((3, value));
    }
    owned_values.push((4, cleaned_exec));

    dedup_search_values(owned_values)
}

pub(crate) fn metadata_fields_for_values<'a>(values: &'a [(u8, String)]) -> Vec<SearchField<'a>> {
    let mut fields = Vec::new();
    for (priority, value) in values {
        dedup_push_search_field(&mut fields, *priority, Some(value.as_str()));
    }
    fields
}

#[allow(dead_code)]
pub(crate) fn search_rank_for_values(
    query: &MetadataQuery,
    values: &[(u8, String)],
) -> Option<SearchRank> {
    let fields = metadata_fields_for_values(values);
    query.search_rank(MetadataCandidate {
        key: "",
        fields: &fields,
        score: 0.0,
    })
}

pub(crate) fn window_search_values(win: &WindowInfo) -> Vec<(u8, String)> {
    let app_key = window_application_key(win);
    let exe_basename = win
        .exe_path
        .as_ref()
        .and_then(|path| path.file_name().and_then(|name| name.to_str()))
        .map(|name| name.to_string());
    let cwd_display = win.cwd_path.as_ref().map(|path| display_path(path));

    let mut owned_values = Vec::new();
    owned_values.push((0, win.title.clone()));
    owned_values.push((1, app_key.clone()));
    if !win.class.eq_ignore_ascii_case(&app_key) {
        owned_values.push((2, win.class.clone()));
    }
    if let Some(value) = win.active_process.clone() {
        owned_values.push((3, value));
    }
    if let Some(value) = win.command_summary.clone() {
        owned_values.push((4, value));
    }
    if let Some(value) = win.command_line.clone() {
        owned_values.push((5, value));
    }
    if let Some(value) = exe_basename {
        owned_values.push((6, value));
    }
    if let Some(value) = cwd_display {
        owned_values.push((7, value));
    }

    dedup_search_values(owned_values)
}
pub(crate) fn sort_ranked_matches_with_visible<T, FVisible, FCompare>(
    items: &mut [T],
    visible_priority_fn: FVisible,
    compare_fn: FCompare,
) where
    FVisible: Fn(&T) -> u8,
    FCompare: Fn(&T, &T) -> std::cmp::Ordering,
{
    items.sort_unstable_by(|left, right| {
        compare_fn(left, right)
            .then_with(|| visible_priority_fn(left).cmp(&visible_priority_fn(right)))
    });
}

pub(crate) fn pinned_app_position(pinned_apps: &[PathBuf], app: &AppInfo) -> usize {
    pinned_apps
        .iter()
        .position(|path| path == &app.desktop_file_path)
        .unwrap_or(usize::MAX)
}

pub(crate) fn clean_exec_cmd(exec: &str) -> String {
    let mut cleaned = exec.to_string();
    for placeholder in &[
        "%f", "%F", "%u", "%U", "%d", "%D", "%n", "%N", "%i", "%c", "%k", "%v",
    ] {
        cleaned = cleaned.replace(placeholder, "");
    }
    cleaned.trim().to_string()
}

pub(crate) fn executable_path_from_exec(exec: &str) -> Option<PathBuf> {
    let command = clean_exec_cmd(exec);
    let executable = command.split_whitespace().next()?.trim_matches('"');
    if executable.is_empty() {
        None
    } else if executable.contains('/') {
        Some(PathBuf::from(executable))
    } else {
        let path_value = std::env::var_os("PATH")?;
        std::env::split_paths(&path_value)
            .map(|directory| directory.join(executable))
            .find(|path| path.is_file())
            .or_else(|| Some(PathBuf::from(executable)))
    }
}

pub(crate) fn is_dolphin_app(app: &AppInfo) -> bool {
    let mut values = vec![app.name.as_str(), app.exec.as_str()];
    if let Some(stem) = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
    {
        values.push(stem);
    }

    values
        .iter()
        .any(|value| normalize_app_match_key(value).contains("dolphin"))
}

pub(crate) fn push_unique_metadata_part(
    parts: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: Option<String>,
) {
    let Some(value) = value.map(|value| value.trim().to_string()) else {
        return;
    };
    if value.is_empty() {
        return;
    }
    let key = normalize_metadata_search_value(&value);
    if key.is_empty() || !seen.insert(key) {
        return;
    }
    parts.push(value);
}

pub(crate) fn app_search_metadata_suffix(app: &AppInfo) -> String {
    let cleaned_exec = clean_exec_cmd(&app.exec);
    let exec_basename = command_basename(&app.exec);
    let desktop_stem = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string());
    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(normalize_metadata_search_value(&app.name));
    push_unique_metadata_part(&mut parts, &mut seen, exec_basename);
    push_unique_metadata_part(&mut parts, &mut seen, desktop_stem);
    push_unique_metadata_part(&mut parts, &mut seen, app.comment.clone());
    push_unique_metadata_part(&mut parts, &mut seen, Some(cleaned_exec));
    parts.join(" | ")
}

pub(crate) fn window_search_metadata_suffix(win: &WindowInfo) -> String {
    let app_key = window_application_key(win);
    let exe_basename = win
        .exe_path
        .as_ref()
        .and_then(|path| path.file_name().and_then(|name| name.to_str()))
        .map(|name| name.to_string());
    let cwd_display = win.cwd_path.as_ref().map(|path| display_path(path));

    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(normalize_metadata_search_value(&win.title));
    push_unique_metadata_part(&mut parts, &mut seen, Some(app_key.clone()));
    if !win.class.eq_ignore_ascii_case(&app_key) {
        push_unique_metadata_part(&mut parts, &mut seen, Some(win.class.clone()));
    }
    push_unique_metadata_part(&mut parts, &mut seen, win.active_process.clone());
    push_unique_metadata_part(&mut parts, &mut seen, win.command_summary.clone());
    push_unique_metadata_part(&mut parts, &mut seen, win.command_line.clone());
    push_unique_metadata_part(&mut parts, &mut seen, exe_basename);
    push_unique_metadata_part(&mut parts, &mut seen, cwd_display);
    parts.join(" | ")
}

pub(crate) fn full_search_visible_app_title(app: &AppInfo) -> String {
    let suffix = app_search_metadata_suffix(app);
    if suffix.is_empty() {
        app.name.clone()
    } else {
        format!("{} | {}", app.name, suffix)
    }
}

pub(crate) fn full_search_visible_window_title(win: &WindowInfo) -> String {
    let suffix = window_search_metadata_suffix(win);
    if suffix.is_empty() {
        win.title.clone()
    } else {
        format!("{} | {}", win.title, suffix)
    }
}

#[allow(dead_code)]
pub(crate) fn ranked_field_value<'a>(
    values: &'a [(u8, String)],
    rank: &SearchRank,
) -> Option<&'a str> {
    values
        .get(rank.provenance().field_index)
        .map(|(_, value)| value.as_str())
}

#[allow(dead_code)]
pub(crate) fn search_visible_app_title_with_rank(
    app: &AppInfo,
    query: &str,
    rank: &SearchRank,
) -> String {
    if query.trim().is_empty() {
        return app.name.clone();
    }
    let values = app_search_values(app);
    let full_text = full_search_visible_app_title(app);
    focus_text_around_match(
        &full_text,
        query,
        ranked_field_value(&values, rank),
        Some(rank),
        70,
    )
}

pub(crate) fn normalize_app_match_key(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

pub(crate) fn push_normalized_key_variants(keys: &mut HashSet<String>, value: &str) {
    let normalized = normalize_app_match_key(value);
    if !normalized.is_empty() {
        keys.insert(normalized);
    }

    for token in value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(normalize_app_match_key)
        .filter(|token| !token.is_empty())
    {
        keys.insert(token);
    }
}

pub(crate) fn window_application_key(win: &WindowInfo) -> String {
    let class = win.class.trim().to_lowercase();
    if !class.is_empty() {
        if let Some(last_segment) = class.rsplit('.').next() {
            if !last_segment.is_empty() {
                return last_segment.to_string();
            }
        }
        return class;
    }

    if let Some(path) = &win.exe_path {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            return name.to_lowercase();
        }
    }

    if let Some(proc_name) = &win.active_process {
        return proc_name.to_lowercase();
    }

    String::new()
}

pub(crate) fn window_grouping_key(win: &WindowInfo) -> String {
    normalize_app_match_key(&window_application_key(win))
}

pub(crate) fn terminal_window_subgroup_key(win: &WindowInfo) -> String {
    if is_terminal_class(&win.class.trim().to_lowercase()) {
        if let Some(proc_name) = win.active_process.as_deref() {
            let primary_title = terminal_primary_title(proc_name, win.command_summary.as_deref());
            let primary_key = normalize_app_match_key(&primary_title);
            if !primary_key.is_empty() {
                return primary_key;
            }

            let proc_key = normalize_app_match_key(proc_name);
            if !proc_key.is_empty() {
                return proc_key;
            }
        }
    }

    window_sort_title_key(win)
}

pub(crate) fn duplicate_window_title_key(win: &WindowInfo) -> Option<String> {
    let mut title = win.title.trim();
    if title.is_empty() {
        return None;
    }

    for separator in [" — ", " – "] {
        if let Some((left, right)) = title.rsplit_once(separator) {
            let suffix_key = normalize_app_match_key(right);
            let app_key = normalize_app_match_key(&window_application_key(win));
            let class_key = normalize_app_match_key(&win.class);
            if !suffix_key.is_empty()
                && (suffix_key == app_key
                    || suffix_key == class_key
                    || class_key.ends_with(&suffix_key))
            {
                title = left.trim();
                break;
            }
        }
    }

    (!title.is_empty()).then(|| title.to_string())
}

pub(crate) fn duplicate_window_group_key(win: &WindowInfo) -> Option<(String, String)> {
    let title = duplicate_window_title_key(win)?;
    let app_key = window_grouping_key(win);
    (!app_key.is_empty()).then_some((app_key, title))
}

pub(crate) fn window_requires_attention(win: &WindowInfo) -> bool {
    win.demands_attention || win.title.to_lowercase().contains("action required")
}

pub(crate) fn update_terminal_attention_schedule(
    enabled: bool,
    eligible_ids: &HashSet<String>,
    deadlines: &mut HashMap<String, Instant>,
    handled: &mut HashSet<String>,
    now: Instant,
) -> (Vec<String>, Option<Instant>) {
    if !enabled {
        deadlines.clear();
        handled.clear();
        return (Vec::new(), None);
    }

    deadlines.retain(|id, _| eligible_ids.contains(id) && !handled.contains(id));
    handled.retain(|id| eligible_ids.contains(id));

    for id in eligible_ids {
        if !handled.contains(id) {
            deadlines
                .entry(id.clone())
                .or_insert(now + Duration::from_secs(AUTO_SEND_ENTER_DELAY_SECS));
        }
    }

    let mut due_ids = Vec::new();
    for id in eligible_ids {
        if deadlines.get(id).is_some_and(|deadline| now >= *deadline) {
            deadlines.remove(id);
            due_ids.push(id.clone());
        }
    }

    let next_deadline = deadlines.values().copied().min();
    (due_ids, next_deadline)
}

pub(crate) fn is_braille_spinner_char(ch: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&ch)
}

pub(crate) fn window_search_metadata_equal(left: &WindowInfo, right: &WindowInfo) -> bool {
    let titles_match = left.title == right.title
        || (left.title.len() == right.title.len()
            && normalize_window_sort_title(&left.title)
                == normalize_window_sort_title(&right.title));
    if !titles_match {
        return false;
    }

    let without_title = |window: &WindowInfo| {
        window_search_values(window)
            .into_iter()
            .filter(|(priority, _)| *priority != 0)
            .collect::<Vec<_>>()
    };

    without_title(left) == without_title(right)
        && window_grouping_key(left) == window_grouping_key(right)
        && terminal_window_subgroup_key(left) == terminal_window_subgroup_key(right)
        && window_sort_title_key(left) == window_sort_title_key(right)
}

const ATTENTION_REQUIRED_FRAMES: [&str; 2] = ["[ . ] Action Required", "[ ! ] Action Required"];

pub(crate) fn attention_required_frame(title: &str) -> Option<&'static str> {
    ATTENTION_REQUIRED_FRAMES
        .into_iter()
        .find(|frame| title.contains(frame))
}

pub(crate) fn refresh_cached_transient_title(
    display_title: &mut String,
    old_title: &str,
    new_title: &str,
) {
    if old_title == new_title || old_title.len() != new_title.len() {
        return;
    }

    if let Some(start) = display_title.find(old_title) {
        display_title.replace_range(start..start + old_title.len(), new_title);
        return;
    }

    if let (Some(old_frame), Some(new_frame)) = (
        attention_required_frame(old_title),
        attention_required_frame(new_title),
    ) {
        *display_title = display_title.replacen(old_frame, new_frame, 1);
    }

    let Some(new_spinner) = new_title.chars().find(|ch| is_braille_spinner_char(*ch)) else {
        return;
    };
    if !old_title.chars().any(is_braille_spinner_char) {
        return;
    }

    *display_title = display_title
        .chars()
        .map(|ch| {
            if is_braille_spinner_char(ch) {
                new_spinner
            } else {
                ch
            }
        })
        .collect();
}

pub(crate) fn normalize_window_sort_title(title: &str) -> String {
    let without_transient_frames = ATTENTION_REQUIRED_FRAMES
        .into_iter()
        .fold(title.to_string(), |normalized, frame| {
            normalized.replace(frame, "Action Required")
        });
    let without_spinners: String = without_transient_frames
        .chars()
        .filter(|ch| !is_braille_spinner_char(*ch))
        .collect();

    if without_spinners.contains(" - ") {
        return without_spinners
            .split(" - ")
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" - ")
            .to_lowercase();
    }

    without_spinners
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn window_sort_title_key(win: &WindowInfo) -> String {
    normalize_window_sort_title(
        &duplicate_window_title_key(win).unwrap_or_else(|| win.title.trim().to_string()),
    )
}

pub(crate) fn command_basename(exec: &str) -> Option<String> {
    let cleaned = clean_exec_cmd(exec);
    let command = cleaned.split_whitespace().next()?;
    let name = Path::new(command).file_name()?.to_str()?;
    Some(name.to_string())
}

pub(crate) fn is_terminal_app_name(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.contains("terminal")
        || lower.contains("konsole")
        || lower.contains("kitty")
        || lower.contains("alacritty")
        || lower.contains("wezterm")
}

pub(crate) fn is_terminal_icon_name(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower == "terminal"
        || lower == "utilities-terminal"
        || lower.ends_with("-terminal")
        || lower.contains("terminal-symbolic")
}

pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    let keep = max_chars.saturating_sub(3);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push_str("...");
    truncated
}

pub(crate) fn focus_match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let query_terms = normalized_query_terms(query);
    if query_terms.len() == 1 {
        let ranked_ranges = valid_match_ranges(text, fuzzy_rank_visible_match_ranges(text, query));
        if !ranked_ranges.is_empty() {
            return ranked_ranges;
        }
    }

    let highlighted_ranges = title_highlight_segments(text, query)
        .into_iter()
        .map(|(start, end, _)| (start, end));
    valid_match_ranges(text, highlighted_ranges)
}

pub(crate) fn ranked_field_focus_ranges(
    text: &str,
    query: &str,
    ranked_field: &str,
) -> Vec<(usize, usize)> {
    let field_ranges = valid_match_ranges(text, title_match_ranges(text, ranked_field));
    if field_ranges.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    for (field_start, field_end) in field_ranges {
        let field_text = &text[field_start..field_end];
        let inner_ranges = focus_match_ranges(field_text, query);
        if inner_ranges.is_empty() {
            ranges.push((field_start, field_end));
            continue;
        }
        ranges.extend(
            inner_ranges
                .into_iter()
                .map(|(start, end)| (field_start + start, field_start + end)),
        );
    }
    valid_match_ranges(text, ranges)
}

pub(crate) fn rank_provenance_ranges_in_field(
    field_text: &str,
    rank: &SearchRank,
) -> Vec<(usize, usize)> {
    let provenance = rank.provenance();
    match provenance.variant_scope {
        Some(1) | Some(2) => alnum_tokens_with_ranges(field_text)
            .into_iter()
            .nth(provenance.token_index)
            .map(|(start, end, _)| vec![(start, end)])
            .unwrap_or_default(),
        _ => char_span_to_byte_range(
            field_text,
            provenance.start_idx,
            provenance.matched_char_len,
        )
        .and_then(|(start, end)| expand_range_to_token_boundaries(field_text, start, end))
        .map(|range| vec![range])
        .unwrap_or_default(),
    }
}

pub(crate) fn ranked_field_rank_ranges(
    text: &str,
    ranked_field: &str,
    rank: &SearchRank,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for (field_start, field_end) in valid_match_ranges(text, title_match_ranges(text, ranked_field))
    {
        let field_text = &text[field_start..field_end];
        ranges.extend(
            rank_provenance_ranges_in_field(field_text, rank)
                .into_iter()
                .map(|(start, end)| (field_start + start, field_start + end)),
        );
    }
    valid_match_ranges(text, ranges)
}

pub(crate) fn ranked_field_rank_token_ranges_in_text(
    text: &str,
    ranked_field: &str,
    rank: &SearchRank,
) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for (start, end) in rank_provenance_ranges_in_field(ranked_field, rank) {
        let Some(token) = ranked_field.get(start..end) else {
            continue;
        };
        ranges.extend(title_match_ranges(text, token));
    }
    valid_match_ranges(text, ranges)
}

pub(crate) fn focus_text_around_match(
    text: &str,
    query: &str,
    ranked_field: Option<&str>,
    rank: Option<&SearchRank>,
    max_chars: usize,
) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    let ranges = ranked_field
        .zip(rank)
        .map(|(field, rank)| ranked_field_rank_ranges(text, field, rank))
        .filter(|ranges| !ranges.is_empty())
        .or_else(|| ranked_field.map(|field| ranked_field_focus_ranges(text, query, field)))
        .filter(|ranges| !ranges.is_empty())
        .unwrap_or_else(|| focus_match_ranges(text, query));
    let Some((match_start_byte, match_end_byte)) = ranges.first().copied() else {
        return truncate_chars(text, max_chars);
    };

    let match_start_char = text[..match_start_byte].chars().count();
    let match_end_char = text[..match_end_byte].chars().count();
    let match_len = match_end_char.saturating_sub(match_start_char).max(1);
    let available_context = max_chars.saturating_sub(match_len);
    let left_context = available_context.min(24);

    let mut start_char = match_start_char.saturating_sub(left_context);
    let mut end_char = (start_char + max_chars).min(char_count);
    if end_char.saturating_sub(start_char) < max_chars {
        start_char = end_char.saturating_sub(max_chars);
    }
    if match_end_char > end_char {
        end_char = match_end_char.min(char_count);
        start_char = end_char.saturating_sub(max_chars);
    }

    let mut result = String::new();
    if start_char > 0 {
        result.push_str("...");
    }
    result.extend(
        text.chars()
            .skip(start_char)
            .take(end_char.saturating_sub(start_char)),
    );
    if end_char < char_count {
        result.push_str("...");
    }
    result
}

pub(crate) fn map_field_highlights_to_full_text(
    full_text: &str,
    fields: &[SearchField<'_>],
    highlights: &[MatchedFieldHighlight],
) -> (Vec<(usize, usize, bool)>, Option<(usize, usize)>) {
    let mut full_ranges = Vec::new();
    let mut strongest_focus: Option<(usize, usize, bool, usize)> = None;

    for hl in highlights {
        let Some(field) = fields.get(hl.field_index) else {
            continue;
        };
        let field_val_lower = field.value.to_lowercase();
        let full_text_lower = full_text.to_lowercase();

        for (match_start, _) in full_text_lower.match_indices(&field_val_lower) {
            for &(r_start, r_end, is_exact) in &hl.ranges {
                let mapped_start = match_start + r_start;
                let mapped_end = match_start + r_end;
                if mapped_end <= full_text.len() {
                    full_ranges.push((mapped_start, mapped_end, is_exact));
                }
            }

            if let Some((f_start, f_end)) = hl.focus_range {
                let mapped_f_start = match_start + f_start;
                let mapped_f_end = match_start + f_end;
                if mapped_f_end <= full_text.len() {
                    let is_exact = hl
                        .ranges
                        .iter()
                        .any(|&(s, e, ex)| s <= f_start && e >= f_end && ex);
                    let priority = if is_exact { 0 } else { 1 };

                    let is_better = match strongest_focus {
                        None => true,
                        Some((_, _, strong_exact, strong_priority)) => {
                            if strong_exact != is_exact {
                                is_exact
                            } else {
                                priority < strong_priority
                            }
                        }
                    };
                    if is_better {
                        strongest_focus = Some((mapped_f_start, mapped_f_end, is_exact, priority));
                    }
                }
            }
        }
    }

    let char_count = full_text.chars().count();
    let mut char_status = vec![None; char_count];

    for (start, end, is_exact) in full_ranges {
        let start_char = full_text[..start].chars().count();
        let end_char = full_text[..end].chars().count();
        for idx in start_char..end_char {
            if idx < char_count {
                if char_status[idx].is_none() || char_status[idx] == Some(false) {
                    char_status[idx] = Some(is_exact);
                }
            }
        }
    }

    let mut merged_ranges = Vec::new();
    let mut current_segment: Option<(usize, bool)> = None;

    for char_idx in 0..char_count {
        let status = char_status[char_idx];
        if let Some(is_exact) = status {
            if let Some((start_char, seg_exact)) = current_segment {
                if seg_exact == is_exact {
                    // Continue segment
                } else {
                    let (byte_start, byte_end) =
                        char_indices_to_byte_range_main(full_text, start_char, char_idx);
                    merged_ranges.push((byte_start, byte_end, seg_exact));
                    current_segment = Some((char_idx, is_exact));
                }
            } else {
                current_segment = Some((char_idx, is_exact));
            }
        } else {
            if let Some((start_char, seg_exact)) = current_segment {
                let (byte_start, byte_end) =
                    char_indices_to_byte_range_main(full_text, start_char, char_idx);
                merged_ranges.push((byte_start, byte_end, seg_exact));
                current_segment = None;
            }
        }
    }
    if let Some((start_char, seg_exact)) = current_segment {
        let (byte_start, byte_end) =
            char_indices_to_byte_range_main(full_text, start_char, char_count);
        merged_ranges.push((byte_start, byte_end, seg_exact));
    }

    let focus = strongest_focus.map(|(s, e, _, _)| (s, e));
    (merged_ranges, focus)
}

pub(crate) fn char_indices_to_byte_range_main(
    text: &str,
    start_char: usize,
    end_char: usize,
) -> (usize, usize) {
    let mut byte_start = 0;
    let mut byte_end = 0;
    for (char_idx, (byte_idx, _)) in text.char_indices().enumerate() {
        if char_idx == start_char {
            byte_start = byte_idx;
        }
        if char_idx == end_char {
            byte_end = byte_idx;
            break;
        }
    }
    if end_char >= text.chars().count() {
        byte_end = text.len();
    }
    (byte_start, byte_end)
}

pub(crate) fn focus_text_around_byte_range(
    text: &str,
    focus: Option<(usize, usize)>,
    highlight_segments: &[(usize, usize, bool)],
    max_chars: usize,
) -> (String, Vec<(usize, usize, bool)>) {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return (text.to_string(), highlight_segments.to_vec());
    }

    let (match_start_char, match_end_char) = if let Some((start_byte, end_byte)) = focus {
        let s_char = text[..start_byte].chars().count();
        let e_char = text[..end_byte].chars().count();
        (s_char, e_char)
    } else {
        (0, 0)
    };

    let match_len = match_end_char.saturating_sub(match_start_char).max(1);
    let available_context = max_chars.saturating_sub(match_len);
    let left_context = available_context.min(24);

    let mut start_char = match_start_char.saturating_sub(left_context);
    let mut end_char = (start_char + max_chars).min(char_count);
    if end_char.saturating_sub(start_char) < max_chars {
        start_char = end_char.saturating_sub(max_chars);
    }
    if match_end_char > end_char {
        end_char = match_end_char.min(char_count);
        start_char = end_char.saturating_sub(max_chars);
    }

    let mut result = String::new();
    if start_char > 0 {
        result.push_str("...");
    }
    let slice_content: String = text
        .chars()
        .skip(start_char)
        .take(end_char - start_char)
        .collect();
    result.push_str(&slice_content);
    if end_char < char_count {
        result.push_str("...");
    }

    let mut adjusted = Vec::new();
    let prefix_len = if start_char > 0 { 3 } else { 0 };

    for &(start_byte, end_byte, is_exact) in highlight_segments {
        let s_char = text[..start_byte].chars().count();
        let e_char = text[..end_byte].chars().count();

        if e_char > start_char && s_char < end_char {
            let visible_s_char = s_char.max(start_char) - start_char;
            let visible_e_char = e_char.min(end_char) - start_char;

            let s_byte = slice_content
                .chars()
                .take(visible_s_char)
                .map(|c| c.len_utf8())
                .sum::<usize>();
            let e_byte = s_byte
                + slice_content
                    .chars()
                    .skip(visible_s_char)
                    .take(visible_e_char - visible_s_char)
                    .map(|c| c.len_utf8())
                    .sum::<usize>();

            adjusted.push((prefix_len + s_byte, prefix_len + e_byte, is_exact));
        }
    }

    (result, adjusted)
}

pub(crate) fn compute_display_title_and_highlights(
    full_text: &str,
    search_values: &[(u8, String)],
    base_query: &MetadataQuery,
    typo_query: &MetadataQuery,
    max_chars: usize,
) -> Option<(SearchRank, String, Vec<(usize, usize, bool)>, bool)> {
    let fields = metadata_fields_for_values(search_values);
    let candidate = MetadataCandidate {
        key: "",
        fields: &fields,
        score: 0.0,
    };
    let base_res = base_query.search_rank_with_highlights(candidate);
    let typo_res = typo_query.search_rank_with_highlights(candidate);

    let (rank, highlights) = match (base_res, typo_res) {
        (Some((base_rank, base_high)), Some((typo_rank, typo_high))) => {
            if pick_better_rank(base_rank.clone(), typo_rank.clone()) == base_rank {
                (base_rank, base_high)
            } else {
                (typo_rank, typo_high)
            }
        }
        (Some((base_rank, base_high)), None) => (base_rank, base_high),
        (None, Some((typo_rank, typo_high))) => (typo_rank, typo_high),
        (None, None) => return None,
    };

    let (full_ranges, focus) = map_field_highlights_to_full_text(full_text, &fields, &highlights);
    let (display_title, highlight_segments) =
        focus_text_around_byte_range(full_text, focus, &full_ranges, max_chars);

    let title_is_typo = highlight_segments.iter().any(|(_, _, is_red)| !*is_red);

    Some((rank, display_title, highlight_segments, title_is_typo))
}
pub(crate) fn best_app_match_score(
    window_keys: &[String],
    app: &AppInfo,
) -> Option<(usize, usize, usize)> {
    let mut best_score: Option<(usize, usize, usize)> = None;

    let mut app_keys = Vec::new();
    app_keys.push(normalize_app_match_key(&app.name));

    if let Some(stem) = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
    {
        app_keys.push(normalize_app_match_key(stem));
    }

    if let Some(exec_name) = command_basename(&app.exec) {
        app_keys.push(normalize_app_match_key(&exec_name));
    }

    app_keys.retain(|key| !key.is_empty());

    for window_key in window_keys {
        for app_key in &app_keys {
            let score = if window_key == app_key {
                Some((0, app_key.len().abs_diff(window_key.len()), app.name.len()))
            } else if app_key.starts_with(window_key) || window_key.starts_with(app_key) {
                Some((1, app_key.len().abs_diff(window_key.len()), app.name.len()))
            } else if app_key.contains(window_key) || window_key.contains(app_key) {
                Some((2, app_key.len().abs_diff(window_key.len()), app.name.len()))
            } else {
                None
            };

            if let Some(score) = score {
                if best_score.is_none_or(|current| score < current) {
                    best_score = Some(score);
                }
            }
        }
    }

    best_score
}

pub(crate) fn truncate_tile_label(text: &str, tile_size: f32) -> String {
    let max_chars = ((tile_size / 7.0).floor() as usize).clamp(6, 22);
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    let keep = max_chars.saturating_sub(3);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push_str("...");
    truncated
}

pub(crate) fn title_match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let mut normalized = String::new();
    let mut mapping = Vec::new();

    for (start, ch) in text.char_indices() {
        let end = start + ch.len_utf8();
        let lower = ch.to_lowercase().collect::<String>();
        let lower_start = normalized.len();
        normalized.push_str(&lower);
        let lower_end = normalized.len();
        mapping.push((lower_start, lower_end, start, end));
    }

    let query_terms = normalize_metadata_search_value(query)
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if query_terms.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();

    for query_term in query_terms {
        for (match_start, _) in normalized.match_indices(&query_term) {
            let match_end = match_start + query_term.len();
            let mut original_start = None;
            let mut original_end = None;

            for (lower_start, lower_end, start, end) in &mapping {
                if *lower_end <= match_start || *lower_start >= match_end {
                    continue;
                }
                original_start.get_or_insert(*start);
                original_end = Some(*end);
            }

            if let (Some(start), Some(end)) = (original_start, original_end) {
                ranges.push((start, end));
            }
        }
    }

    ranges.sort_by_key(|(start, end)| (*start, *end));
    let mut merged = Vec::new();
    for (start, end) in ranges {
        if let Some((_, previous_end)) = merged.last_mut() {
            if start <= *previous_end {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    merged
}

pub(crate) fn normalized_query_terms(query: &str) -> Vec<String> {
    normalize_metadata_search_value(query)
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

pub(crate) fn alnum_tokens_with_ranges(text: &str) -> Vec<(usize, usize, String)> {
    let mut tokens = Vec::new();
    let mut token_start = None;

    for (idx, ch) in text.char_indices() {
        if ch.is_ascii_alphanumeric() {
            token_start.get_or_insert(idx);
            continue;
        }
        if let Some(start) = token_start.take() {
            let end = idx;
            let normalized = normalize_metadata_search_value(&text[start..end]).to_lowercase();
            if !normalized.is_empty() {
                tokens.push((start, end, normalized));
            }
        }
    }

    if let Some(start) = token_start {
        let end = text.len();
        let normalized = normalize_metadata_search_value(&text[start..end]).to_lowercase();
        if !normalized.is_empty() {
            tokens.push((start, end, normalized));
        }
    }

    tokens
}

pub(crate) fn char_span_to_byte_range(
    text: &str,
    start_idx: usize,
    char_len: usize,
) -> Option<(usize, usize)> {
    if char_len == 0 {
        return None;
    }
    let start = text
        .char_indices()
        .nth(start_idx)
        .map(|(idx, _)| idx)
        .or_else(|| (start_idx == text.chars().count()).then_some(text.len()))?;
    let end_char_idx = start_idx.saturating_add(char_len);
    let end = text
        .char_indices()
        .nth(end_char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    (start < end).then_some((start, end))
}

pub(crate) fn valid_byte_range(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    (start < end && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end))
        .then_some((start, end))
}

pub(crate) fn valid_match_ranges<I>(text: &str, ranges: I) -> Vec<(usize, usize)>
where
    I: IntoIterator<Item = (usize, usize)>,
{
    ranges
        .into_iter()
        .filter_map(|(start, end)| valid_byte_range(text, start, end))
        .collect()
}

pub(crate) fn expand_range_to_token_boundaries(
    text: &str,
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    let (mut start, mut end) = valid_byte_range(text, start, end)?;

    while start > 0 {
        let Some((previous_idx, previous_ch)) = text[..start].char_indices().next_back() else {
            break;
        };
        if !previous_ch.is_ascii_alphanumeric() {
            break;
        }
        start = previous_idx;
    }

    while end < text.len() {
        let Some(next_ch) = text[end..].chars().next() else {
            break;
        };
        if !next_ch.is_ascii_alphanumeric() {
            break;
        }
        end += next_ch.len_utf8();
    }

    valid_byte_range(text, start, end)
}

pub(crate) fn fuzzy_rank_visible_match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let Some(query) = MetadataQuery::new(query).map(|query| query.with_typo_fallback(true)) else {
        return Vec::new();
    };
    let field = SearchField {
        priority: 0,
        value: text,
    };
    let fields = [field];
    let candidate = MetadataCandidate {
        key: "",
        fields: &fields,
        score: 0.0,
    };
    let Some(rank) = query.search_rank(candidate) else {
        return Vec::new();
    };
    let provenance = rank.provenance();

    match provenance.variant_scope {
        Some(1) => alnum_tokens_with_ranges(text)
            .into_iter()
            .nth(provenance.token_index)
            .map(|(token_start, token_end, _)| vec![(token_start, token_end)])
            .unwrap_or_default(),
        Some(2) => alnum_tokens_with_ranges(text)
            .into_iter()
            .nth(provenance.token_index)
            .map(|(start, end, _)| vec![(start, end)])
            .unwrap_or_default(),
        _ => char_span_to_byte_range(text, provenance.start_idx, provenance.matched_char_len)
            .and_then(|(start, end)| expand_range_to_token_boundaries(text, start, end))
            .map(|range| vec![range])
            .unwrap_or_default(),
    }
}

pub(crate) fn bounded_damerau_levenshtein(left: &str, right: &str, limit: usize) -> Option<usize> {
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    if left_chars.len().abs_diff(right_chars.len()) > limit {
        return None;
    }

    let mut previous_previous = Vec::new();
    let mut previous: Vec<usize> = (0..=right_chars.len()).collect();
    let mut current = vec![0; right_chars.len() + 1];

    for (left_idx, left_ch) in left_chars.iter().enumerate() {
        current[0] = left_idx + 1;
        let mut row_min = current[0];
        for (right_idx, right_ch) in right_chars.iter().enumerate() {
            let cost = usize::from(left_ch != right_ch);
            let deletion = previous[right_idx + 1] + 1;
            let insertion = current[right_idx] + 1;
            let substitution = previous[right_idx] + cost;
            let mut value = deletion.min(insertion).min(substitution);
            if left_idx > 0
                && right_idx > 0
                && *left_ch == right_chars[right_idx - 1]
                && left_chars[left_idx - 1] == *right_ch
            {
                value = value.min(previous_previous[right_idx - 1] + 1);
            }
            current[right_idx + 1] = value;
            row_min = row_min.min(value);
        }

        if row_min > limit {
            return None;
        }
        previous_previous.clone_from(&previous);
        std::mem::swap(&mut previous, &mut current);
    }

    let distance = previous[right_chars.len()];
    (distance <= limit).then_some(distance)
}

pub(crate) fn visible_typo_distance(query: &str, candidate: &str) -> Option<(usize, usize)> {
    if query.is_empty() || candidate.is_empty() || query == candidate {
        return None;
    }
    let query_len = query.chars().count();
    let candidate_len = candidate.chars().count();
    let limit = (query_len / 2).max(1);
    let distance = bounded_damerau_levenshtein(query, candidate, limit)?;
    let ratio = distance * 1000 / query_len.max(candidate_len);
    Some((distance, ratio))
}

pub(crate) fn best_visible_typo_match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let normalized_query = normalize_metadata_search_value(query).to_lowercase();
    if normalized_query.is_empty() {
        return Vec::new();
    }
    let tokens = alnum_tokens_with_ranges(text);
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut best: Option<(usize, usize, usize, usize, String)> = None;
    for span_len in 1..=4 {
        for start in 0..tokens.len() {
            if start + span_len > tokens.len() {
                break;
            }
            let mut combined = String::new();
            for token in tokens.iter().skip(start).take(span_len) {
                combined.push_str(&token.2);
            }
            let Some((distance, ratio)) = visible_typo_distance(&normalized_query, &combined)
            else {
                continue;
            };
            let is_better = best.as_ref().is_none_or(
                |(best_distance, best_ratio, best_span_len, best_start, ..)| {
                    (distance, ratio, span_len, start)
                        < (*best_distance, *best_ratio, *best_span_len, *best_start)
                },
            );
            if is_better {
                best = Some((distance, ratio, span_len, start, combined));
            }
        }
        if best.is_some() {
            break;
        }
    }

    let Some((_, _, span_len, _, best_key)) = best else {
        return Vec::new();
    };

    let mut ranges = Vec::new();
    for start in 0..tokens.len() {
        if start + span_len > tokens.len() {
            break;
        }
        let mut combined = String::new();
        for idx in start..start + span_len {
            combined.push_str(&tokens[idx].2);
        }
        if combined == best_key {
            ranges.push((tokens[start].0, tokens[start + span_len - 1].1));
        }
    }
    ranges
}

pub(crate) fn typo_title_match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let ranges = fuzzy_rank_visible_match_ranges(text, query);
    let ranges = valid_match_ranges(text, ranges);
    if ranges.is_empty() {
        best_visible_typo_match_ranges(text, query)
    } else {
        ranges
    }
}

pub(crate) fn title_highlight_segments(text: &str, query: &str) -> Vec<(usize, usize, bool)> {
    let query_terms = normalized_query_terms(query);
    let mut red_ranges = Vec::new();
    let mut matched_terms = HashSet::new();
    for term in &query_terms {
        let ranges = valid_match_ranges(text, title_match_ranges(text, term));
        if !ranges.is_empty() {
            matched_terms.insert(term.clone());
            red_ranges.extend(ranges);
        }
    }
    red_ranges.sort_by_key(|(start, end)| (*start, *end));
    let mut merged_red_ranges = Vec::new();
    for (start, end) in red_ranges {
        if let Some((_, previous_end)) = merged_red_ranges.last_mut() {
            if start <= *previous_end {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        merged_red_ranges.push((start, end));
    }

    let mut yellow_ranges = Vec::new();
    for term in &query_terms {
        if matched_terms.contains(term) {
            continue;
        }
        for (start, end) in valid_match_ranges(text, typo_title_match_ranges(text, term)) {
            let overlaps_red = merged_red_ranges
                .iter()
                .any(|(red_start, red_end)| start < *red_end && end > *red_start);
            if !overlaps_red {
                yellow_ranges.push((start, end));
            }
        }
    }
    yellow_ranges.sort_by_key(|(start, end)| (*start, *end));
    let mut merged_yellow_ranges = Vec::new();
    for (start, end) in yellow_ranges {
        if let Some((_, previous_end)) = merged_yellow_ranges.last_mut() {
            if start <= *previous_end {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        merged_yellow_ranges.push((start, end));
    }

    let mut segments = Vec::new();
    for (start, end) in merged_red_ranges {
        segments.push((start, end, true));
    }
    for (start, end) in merged_yellow_ranges {
        segments.push((start, end, false));
    }
    segments.sort_by_key(|(start, end, is_red)| (*start, *end, !*is_red));

    segments
}

pub(crate) fn title_highlight_segments_with_ranked_field(
    text: &str,
    query: &str,
    ranked_field: Option<&str>,
    rank: Option<&SearchRank>,
) -> Vec<(usize, usize, bool)> {
    let mut segments = title_highlight_segments(text, query);
    let Some((ranked_field, rank)) = ranked_field.zip(rank) else {
        return segments;
    };

    let field_ranges = ranked_field_rank_token_ranges_in_text(text, ranked_field, rank);
    for (start, end) in field_ranges {
        let already_highlighted = segments
            .iter()
            .any(|(segment_start, segment_end, _)| start < *segment_end && end > *segment_start);
        if !already_highlighted {
            segments.push((start, end, false));
        }
    }
    segments.sort_by_key(|(start, end, is_red)| (*start, *end, !*is_red));
    segments
}

pub(crate) fn highlighted_title_job_from_segments(
    text: &str,
    font_size: f32,
    segments: &[(usize, usize, bool)],
) -> egui::text::LayoutJob {
    let default_format = egui::TextFormat {
        font_id: egui::FontId::proportional(font_size),
        color: egui::Color32::WHITE,
        ..Default::default()
    };
    let highlight_format = egui::TextFormat {
        font_id: egui::FontId::proportional(font_size),
        color: egui::Color32::from_rgb(235, 90, 90),
        ..Default::default()
    };
    let typo_highlight_format = egui::TextFormat {
        font_id: egui::FontId::proportional(font_size),
        color: egui::Color32::from_rgb(235, 196, 72),
        ..Default::default()
    };

    let mut job = egui::text::LayoutJob::default();

    let valid_segments: Vec<_> = segments
        .iter()
        .copied()
        .filter_map(|(start, end, is_red)| {
            valid_byte_range(text, start, end).map(|(start, end)| (start, end, is_red))
        })
        .collect();

    if valid_segments.is_empty() {
        job.append(text, 0.0, default_format);
        return job;
    }

    let mut cursor = 0usize;
    for (start, end, is_red) in valid_segments {
        if start < cursor {
            continue;
        }
        if cursor < start {
            job.append(&text[cursor..start], 0.0, default_format.clone());
        }
        job.append(
            &text[start..end],
            0.0,
            if is_red {
                highlight_format.clone()
            } else {
                typo_highlight_format.clone()
            },
        );
        cursor = end;
    }
    if cursor < text.len() {
        job.append(&text[cursor..], 0.0, default_format);
    }

    job
}

pub(crate) fn pick_better_rank(left: SearchRank, right: SearchRank) -> SearchRank {
    if left <= right { left } else { right }
}

#[allow(dead_code)]
pub(crate) fn visible_title_has_typo_match(title: &str, query: &str) -> bool {
    if query.trim().is_empty() || !title_match_ranges(title, query).is_empty() {
        return false;
    }
    !typo_title_match_ranges(title, query).is_empty()
}

pub(crate) fn visible_match_priority(title: &str, query: &str) -> u8 {
    if query.trim().is_empty() {
        0
    } else if !title_match_ranges(title, query).is_empty() {
        0
    } else {
        1
    }
}
