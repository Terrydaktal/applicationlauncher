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

pub(crate) fn terminal_title_segments(
    dynamic_title: &str,
    proc_name: &str,
    command_summary: Option<&str>,
    cwd: Option<&str>,
    parent_program: Option<&str>,
) -> Vec<String> {
    let dynamic_title = dynamic_title.trim();
    let mut segments = Vec::new();
    let primary_title = terminal_primary_title(proc_name, command_summary);
    if let Some(parent_program) = parent_program {
        push_unique_terminal_segment(&mut segments, parent_program);
        push_unique_terminal_segment(&mut segments, terminal_process_display_name(proc_name));
    } else {
        push_unique_terminal_segment(&mut segments, primary_title.clone());
    }

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

    for part in title_parts {
        let part = part.trim();
        if part.is_empty() || is_terminal_title_marker(part) {
            continue;
        }

        if prefer_cwd_over_dynamic_path && terminal_context_looks_path_like(part) {
            continue;
        }

        if let Some(cwd) = cwd.filter(|cwd| terminal_segment_matches_cwd_basename(part, cwd)) {
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

    segments.push("Terminal".to_string());
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

    if normalize_app_match_key(proc_name) == "ssh"
        && let Some(title) = ssh_terminal_display_title(raw_title, parent_program)
    {
        return title;
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
            )
            .join(sep);
        }
    }

    terminal_title_segments(raw_title, proc_name, command_summary, cwd, parent_program).join(" - ")
}

fn ssh_terminal_display_title(raw_title: &str, parent_program: Option<&str>) -> Option<String> {
    let dynamic_title = strip_terminal_title_marker(raw_title);
    let closing_bracket = dynamic_title.find(']')?;
    if !dynamic_title.starts_with('[') {
        return None;
    }

    let remote_user = dynamic_title[..=closing_bracket].trim();
    let remote_context = dynamic_title[closing_bracket + 1..].trim();
    if remote_user.len() <= 2 || remote_context.is_empty() {
        return None;
    }

    let remote_shell = parent_program
        .filter(|program| is_shell_process(program))
        .map(terminal_process_display_name);
    let remote_identity = remote_shell
        .map(|shell| format!("{remote_user} {shell}"))
        .unwrap_or_else(|| remote_user.to_string());

    Some(format!(
        "ssh - {remote_identity} - {remote_context} - Terminal"
    ))
}

fn strip_terminal_title_marker(raw_title: &str) -> &str {
    for separator in [" - ", " — ", " – ", " : ", " | "] {
        if let Some(dynamic_title) = raw_title
            .strip_suffix("Terminal")
            .and_then(|title| title.strip_suffix(separator))
        {
            return dynamic_title.trim();
        }
    }

    raw_title.trim()
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
