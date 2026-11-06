use crate::models::ProcessChainEntry;
use crate::{is_braille_spinner_char, normalize_app_match_key};

pub(crate) fn is_generic_terminal_process(proc_name: &str) -> bool {
    matches!(
        normalize_app_match_key(proc_name).as_str(),
        "bash" | "fish" | "sh" | "zsh" | "python" | "python3" | "node" | "ruby" | "perl"
    )
}

pub(crate) fn terminal_process_display_name(proc_name: &str) -> &str {
    if is_codex_process(proc_name) {
        "codex"
    } else {
        proc_name.trim()
    }
}

pub(crate) fn is_codex_process(proc_name: &str) -> bool {
    let normalized = normalize_app_match_key(proc_name);
    normalized == "codex" || normalized.starts_with("codexcodemode")
}

pub(crate) fn terminal_parent_program<'a>(
    proc_name: &str,
    process_chain: &'a [ProcessChainEntry],
) -> Option<&'a str> {
    if is_codex_process(proc_name) {
        return None;
    }

    if process_chain
        .iter()
        .skip(1)
        .any(|entry| is_codex_process(&entry.name))
    {
        return Some("codex");
    }

    if normalize_app_match_key(proc_name) == "ssh" {
        return process_chain
            .iter()
            .skip(1)
            .find(|entry| is_shell_process(&entry.name))
            .map(|entry| terminal_process_display_name(&entry.name));
    }

    None
}

fn is_shell_process(proc_name: &str) -> bool {
    matches!(
        normalize_app_match_key(proc_name).as_str(),
        "bash" | "fish" | "sh" | "zsh"
    )
}

pub(crate) fn terminal_primary_title(proc_name: &str, command_summary: Option<&str>) -> String {
    let proc_name = proc_name.trim();
    let Some(command_summary) = command_summary
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return terminal_process_display_name(proc_name).to_string();
    };

    if is_generic_terminal_process(proc_name)
        && normalize_app_match_key(command_summary) != normalize_app_match_key(proc_name)
    {
        command_summary.to_string()
    } else {
        terminal_process_display_name(proc_name).to_string()
    }
}

pub(crate) fn terminal_context_looks_path_like(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && (trimmed.starts_with('~')
            || trimmed.starts_with('/')
            || trimmed == "."
            || trimmed == ".."
            || trimmed.contains('/'))
}

pub(crate) fn strip_leading_braille_spinner(value: &str) -> &str {
    value.trim_start_matches(|ch: char| is_braille_spinner_char(ch) || ch.is_whitespace())
}

pub(crate) fn terminal_segment_leading_spinner(value: &str) -> Option<char> {
    value
        .trim_start()
        .chars()
        .next()
        .filter(|ch| is_braille_spinner_char(*ch))
}

pub(crate) fn terminal_path_basename(value: &str) -> Option<&str> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    trimmed.rsplit('/').next().filter(|part| !part.is_empty())
}

pub(crate) fn terminal_segment_matches_cwd_basename(part: &str, cwd: &str) -> bool {
    let part = strip_leading_braille_spinner(part).trim();
    let Some(cwd_basename) = terminal_path_basename(cwd) else {
        return false;
    };

    !part.is_empty() && normalize_app_match_key(part) == normalize_app_match_key(cwd_basename)
}

pub(crate) fn is_terminal_title_marker(value: &str) -> bool {
    let key = normalize_app_match_key(value);
    key == "terminal"
        || key == "xfce4terminal"
        || key == "konsole"
        || key == "kitty"
        || key == "alacritty"
        || key == "wezterm"
        || key == "foot"
        || key.ends_with("terminal")
}

fn is_pi_title_marker(value: &str) -> bool {
    unstyled_title_characters(value).eq("pi".chars())
        || unstyled_title_characters(value).eq("\u{03c0}".chars())
}

pub(crate) fn terminal_title_segments(
    dynamic_title: &str,
    proc_name: &str,
    command_summary: Option<&str>,
    cwd: Option<&str>,
    parent_program: Option<&str>,
    terminal_marker: &str,
) -> Vec<String> {
    let dynamic_title = dynamic_title.trim();
    let mut segments = Vec::new();
    let primary_title = terminal_primary_title(proc_name, command_summary);
    let primary_key = normalize_app_match_key(&primary_title);
    let proc_key = normalize_app_match_key(proc_name);
    let parent_key = parent_program.map(normalize_app_match_key);
    let cwd = cwd.map(str::trim).filter(|value| !value.is_empty());
    let cwd_key = cwd.map(normalize_app_match_key);
    let separators = [" - ", " — ", " – ", " : ", " | "];
    let mut has_cwd_segment = false;
    let prefer_cwd_over_dynamic_path =
        cwd.is_some() && terminal_context_looks_path_like(dynamic_title);

    let title_parts: Vec<&str> = separators
        .iter()
        .find_map(|sep| {
            dynamic_title
                .contains(sep)
                .then(|| dynamic_title.split(sep).map(str::trim).collect())
        })
        .unwrap_or_else(|| {
            if dynamic_title.is_empty() {
                Vec::new()
            } else {
                vec![dynamic_title]
            }
        });

    let has_separate_title_parts = title_parts.len() > 1;
    let pi_marker = if primary_key == "pi" {
        title_parts
            .iter()
            .copied()
            .find(|part| {
                !part.is_empty() && crate::search::attention_required_frame(part) != Some(*part)
            })
            .filter(|part| is_pi_title_marker(part))
    } else {
        None
    };
    // Pi's one-character glyph carries the style of the complete program name.
    let primary_title = pi_marker
        .and_then(|marker| {
            marker
                .chars()
                .find(|ch| crate::search::is_title_strikethrough_mark(*ch))
        })
        .map(|mark| format!("p{mark}i{mark}"))
        .unwrap_or(primary_title);
    if let Some(parent_program) = parent_program {
        push_unique_terminal_segment(&mut segments, parent_program);
        if proc_key == "pi" {
            push_unique_terminal_segment(&mut segments, primary_title);
        } else {
            push_unique_terminal_segment(&mut segments, terminal_process_display_name(proc_name));
        }
    } else {
        push_unique_terminal_segment(&mut segments, primary_title);
    }

    for part in title_parts {
        let part = part.trim();
        if part.is_empty() || is_terminal_title_marker(part) {
            continue;
        }

        // Resolve separate directory fields before the legacy whole-caption fallback
        // can reinsert a program marker or task that has already been handled.
        if let Some(cwd) = cwd.filter(|cwd| {
            terminal_segment_matches_cwd_basename(part, cwd)
                || (has_separate_title_parts && terminal_context_looks_path_like(part))
        }) {
            let spinner = terminal_segment_leading_spinner(part).filter(|_| {
                !segments
                    .iter()
                    .any(|segment| segment.chars().any(is_braille_spinner_char))
            });
            let cwd_segment = spinner
                .map(|spinner| format!("{spinner} {cwd}"))
                .unwrap_or_else(|| cwd.to_string());
            push_unique_terminal_segment(&mut segments, cwd_segment);
            has_cwd_segment = true;
            continue;
        }

        if prefer_cwd_over_dynamic_path && terminal_context_looks_path_like(part) {
            continue;
        }

        let part_key = normalize_app_match_key(part);
        if part_key.is_empty()
            || part_key == primary_key
            || part_key == proc_key
            || parent_key.as_ref().is_some_and(|key| *key == part_key)
        {
            continue;
        }

        if cwd_key.as_ref().is_some_and(|cwd_key| *cwd_key == part_key) {
            has_cwd_segment = true;
        }

        push_unique_terminal_segment(&mut segments, part.to_string());
    }

    if let Some(cwd) = cwd {
        if !has_cwd_segment {
            let cwd_context = if dynamic_title.is_empty()
                || parent_program.is_some()
                || !terminal_context_looks_path_like(dynamic_title)
            {
                cwd.to_string()
            } else {
                replace_terminal_suffix_path(dynamic_title, cwd)
            };
            push_unique_terminal_segment(&mut segments, cwd_context);
        }
    }

    segments.push(terminal_marker.to_string());
    segments
}

pub(crate) fn terminal_display_title(
    raw_title: &str,
    proc_name: &str,
    command_summary: Option<&str>,
    cwd: Option<&str>,
    parent_program: Option<&str>,
) -> String {
    let separators = [" - ", " — ", " – ", " : ", " | "];
    let raw_title = raw_title
        .trim()
        .strip_prefix("- ")
        .unwrap_or(raw_title.trim());

    let (dynamic_title, terminal_marker, separator) = split_terminal_title_marker(raw_title);
    // These titles already contain their program/context in the user's selected order.
    if dynamic_title.starts_with("tmux: ")
        || (is_codex_process(proc_name) && is_configured_codex_title(dynamic_title))
    {
        return format!("{dynamic_title} - {terminal_marker}");
    }

    if normalize_app_match_key(proc_name) == "ssh"
        && let Some(title) = ssh_terminal_display_title(raw_title, parent_program)
    {
        return title;
    }

    if let Some(segments) = command_directory_title_segments(
        dynamic_title,
        proc_name,
        command_summary,
        cwd,
        parent_program,
        terminal_marker,
    ) {
        return segments.join(separator);
    }

    for sep in separators {
        let parts: Vec<&str> = raw_title.split(sep).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }

        if parts
            .first()
            .is_some_and(|part| is_terminal_title_marker(part))
        {
            let suffix = parts[1..].join(sep);
            return terminal_title_segments(
                &suffix,
                proc_name,
                command_summary,
                cwd,
                parent_program,
                terminal_marker,
            )
            .join(sep);
        }

        if parts
            .last()
            .is_some_and(|part| is_terminal_title_marker(part))
        {
            let suffix = parts[..parts.len() - 1].join(sep);
            return terminal_title_segments(
                &suffix,
                proc_name,
                command_summary,
                cwd,
                parent_program,
                terminal_marker,
            )
            .join(sep);
        }
    }

    terminal_title_segments(
        raw_title,
        proc_name,
        command_summary,
        cwd,
        parent_program,
        terminal_marker,
    )
    .join(" - ")
}

fn command_directory_fields(value: &str) -> Option<(&str, &str)> {
    let (program, context) = value.split_once(char::is_whitespace)?;
    let context = context.trim_start();
    let path = strip_leading_braille_spinner(context);
    (path.starts_with(['~', '/'])
        || matches!(path, "." | "..")
        || path.starts_with("./")
        || path.starts_with("../"))
    .then_some((program, context))
}

fn command_directory_title_segments(
    dynamic_title: &str,
    proc_name: &str,
    command_summary: Option<&str>,
    cwd: Option<&str>,
    parent_program: Option<&str>,
    terminal_marker: &str,
) -> Option<Vec<String>> {
    let (program, context) = command_directory_fields(dynamic_title)?;
    // Agent captions can carry a task after the path, unlike Fish's directory-only field.
    if (unstyled_title_characters(program).eq("codex".chars())
        || unstyled_title_characters(program).eq("pi".chars()))
        && [" - ", " — ", " – ", " : ", " | "]
            .iter()
            .any(|separator| context.contains(separator))
    {
        return None;
    }
    let matches_process = unstyled_title_characters(program).eq(unstyled_title_characters(
        terminal_process_display_name(proc_name),
    ));
    let matches_summary = command_summary.is_some_and(|summary| {
        unstyled_title_characters(program).eq(unstyled_title_characters(summary.trim()))
    });
    if !matches_process && !matches_summary {
        return None;
    }
    let primary = terminal_primary_title(proc_name, command_summary);
    let mut segments = Vec::new();
    if let Some(parent) = parent_program {
        push_unique_terminal_segment(&mut segments, parent);
        push_unique_terminal_segment(&mut segments, program);
    } else if unstyled_title_characters(program).eq(unstyled_title_characters(&primary)) {
        push_unique_terminal_segment(&mut segments, program);
    } else {
        push_unique_terminal_segment(&mut segments, primary);
    }
    // Fish's command and directory share one field; spaces and separators can belong to the path.
    let directory = match cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) {
        Some(cwd) => terminal_segment_leading_spinner(context)
            .map(|spinner| format!("{spinner} {cwd}"))
            .unwrap_or_else(|| cwd.to_owned()),
        None => context.to_owned(),
    };
    push_unique_terminal_segment(&mut segments, directory);
    segments.push(terminal_marker.to_owned());
    Some(segments)
}

pub(crate) fn is_configured_codex_title(title: &str) -> bool {
    [" | ", " - "].into_iter().any(|separator| {
        title.split(separator).any(|part| {
            let mut characters = unstyled_title_characters(part);
            characters.by_ref().take(5).eq("codex".chars())
                && match characters.next() {
                    None => title.contains(separator),
                    Some(' ') => characters.next().is_some_and(is_braille_spinner_char),
                    _ => false,
                }
        })
    })
}

pub(crate) fn tmux_codex_display_title(pane_title: &str, cwd: &str) -> String {
    let spinner = pane_title.chars().find(|ch| is_braille_spinner_char(*ch));
    let attention = crate::search::attention_required_frame(pane_title);
    let mut clean = String::with_capacity(pane_title.len());
    for (index, part) in pane_title.split(is_braille_spinner_char).enumerate() {
        clean.push_str(if index == 0 {
            part
        } else {
            part.trim_start_matches(' ')
        });
    }
    if clean.contains(" - ") {
        clean = clean.replace(" - ", " | ");
    }
    let mut body = clean.trim();
    if let Some(marker) = attention
        && let Some(rest) = body.strip_prefix(marker)
    {
        body = rest
            .strip_prefix(" | ")
            .or_else(|| rest.strip_prefix(" - "))
            .unwrap_or(rest)
            .trim_start_matches(' ');
    }
    // New selections put app/cwd first; legacy selections put the project last.
    let configured_context = body
        .strip_prefix("codex | ")
        .or_else(|| body.strip_prefix("codex "))
        .filter(|rest| rest.starts_with(['~', '/']));
    let task = if let Some(rest) = configured_context {
        rest.split_once(" | ").map_or("", |(_, task)| task)
    } else if terminal_path_basename(cwd).is_some_and(|basename| body == basename) {
        ""
    } else {
        body.rsplit_once(" | ").map_or(body, |(task, _)| task)
    };

    let mut title = String::from("codex: ");
    if let Some(spinner) = spinner {
        title.push(spinner);
        title.push(' ');
    }
    if let Some(marker) = attention {
        title.push_str(marker);
        title.push(' ');
    }
    title.push_str(cwd);
    if !task.is_empty() {
        title.push_str(" - ");
        title.push_str(&task.replace(" | ", " - "));
    }
    title.push_str(" - Terminal");
    title
}

fn ssh_terminal_display_title(raw_title: &str, parent_program: Option<&str>) -> Option<String> {
    let (dynamic_title, terminal_marker, _) = split_terminal_title_marker(raw_title);
    let closing_bracket = dynamic_title.find(']')?;
    if !dynamic_title.starts_with('[') {
        return None;
    }

    let remote_user = dynamic_title[..=closing_bracket].trim();
    let remote_context = dynamic_title[closing_bracket + 1..].trim();
    if remote_user.len() <= 2 || remote_context.is_empty() {
        return None;
    }

    if let Some((shell, directory)) =
        command_directory_fields(remote_context).filter(|(program, _)| is_shell_process(program))
    {
        return Some(format!(
            "ssh - {remote_user} {shell} - {directory} - {terminal_marker}"
        ));
    }

    let remote_shell = parent_program
        .filter(|program| is_shell_process(program))
        .map(terminal_process_display_name);
    let remote_identity = remote_shell
        .map(|shell| format!("{remote_user} {shell}"))
        .unwrap_or_else(|| remote_user.to_string());

    Some(format!(
        "ssh - {remote_identity} - {remote_context} - {terminal_marker}"
    ))
}

fn unstyled_title_characters(value: &str) -> impl Iterator<Item = char> + '_ {
    value
        .chars()
        .filter(|character| !crate::search::is_title_strikethrough_mark(*character))
}

fn split_terminal_title_marker(raw_title: &str) -> (&str, &str, &str) {
    for separator in [" - ", " — ", " – ", " : ", " | "] {
        // Match undecorated text, but retain the original marker for rendering.
        if let Some((dynamic_title, marker)) = raw_title.rsplit_once(separator)
            && unstyled_title_characters(marker).eq("Terminal".chars())
        {
            return (dynamic_title.trim(), marker, separator);
        }
        if let Some((marker, dynamic_title)) = raw_title.split_once(separator)
            && unstyled_title_characters(marker).eq("Terminal".chars())
        {
            return (dynamic_title.trim(), marker, separator);
        }
    }

    (raw_title.trim(), "Terminal", " - ")
}

pub(crate) fn replace_terminal_suffix_path(original_suffix: &str, cwd: &str) -> String {
    let trimmed = original_suffix.trim();
    if trimmed.is_empty() {
        return cwd.to_string();
    }

    match trimmed.rfind(char::is_whitespace) {
        Some(split_at) => format!("{} {}", trimmed[..split_at].trim_end(), cwd),
        None => cwd.to_string(),
    }
}

pub(crate) fn push_unique_terminal_segment(segments: &mut Vec<String>, value: impl Into<String>) {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }

    let key = normalize_app_match_key(trimmed);
    let already_present = if key.is_empty() {
        segments
            .iter()
            .any(|existing| existing.trim().eq_ignore_ascii_case(trimmed))
    } else {
        segments
            .iter()
            .any(|existing| normalize_app_match_key(existing) == key)
    };
    if already_present {
        return;
    }

    segments.push(trimmed.to_string());
}

pub(crate) fn normalize_terminal_title_marker_position(raw_title: &str) -> String {
    let separators = [" - ", " — ", " – ", " : ", " | "];

    for sep in separators {
        let parts: Vec<&str> = raw_title.split(sep).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }

        let first_is_marker = parts
            .first()
            .is_some_and(|part| is_terminal_title_marker(part));
        let last_is_marker = parts
            .last()
            .is_some_and(|part| is_terminal_title_marker(part));

        if first_is_marker && last_is_marker && parts.len() >= 3 {
            return parts[1..].join(sep);
        }

        if first_is_marker {
            let body = parts[1..]
                .iter()
                .copied()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            if !body.is_empty() {
                let mut rebuilt = body;
                rebuilt.push("Terminal");
                return rebuilt.join(sep);
            }
        }
    }

    raw_title.trim().to_string()
}
