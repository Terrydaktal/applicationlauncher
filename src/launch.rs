use std::path::{Path, PathBuf};
use std::process::Command;

use super::{
    ATSPI_LOCATION_PROBE, clean_exec_cmd, command_basename, is_terminal_app_name,
    is_terminal_icon_name, lookup_theme_icon_exact, normalize_app_match_key,
};
use crate::models::{AppInfo, WindowInfo};

pub(crate) fn launch_app(exec: &str) {
    let cmd_str = clean_exec_cmd(exec);
    std::thread::spawn(move || {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&cmd_str);

        // Clean Python environment variables to prevent version mismatch crashes in launched apps
        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("PYTHONHOME");

        if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
            cmd.env_remove("VIRTUAL_ENV");
            if let Ok(path_val) = std::env::var("PATH") {
                let venv_bin = std::path::PathBuf::from(venv).join("bin");
                let new_paths: Vec<_> = std::env::split_paths(&path_val)
                    .filter(|p| p != &venv_bin)
                    .collect();
                if let Ok(joined) = std::env::join_paths(new_paths) {
                    cmd.env("PATH", joined);
                }
            }
        }

        let _ = cmd.spawn();
    });
}

pub(crate) fn launch_terminal_cd(target: &str) {
    let target = target.trim();
    if target.is_empty() {
        return;
    }

    launch_fish_terminal(Some(target.to_string()), None, None);
}

pub(crate) fn launch_terminal_command(command: &str) {
    let command = command.trim();
    if command.is_empty() {
        return;
    }

    let command = command.to_string();
    std::thread::spawn(move || {
        let mut cmd = Command::new("xfce4-terminal");
        cmd.arg("--command")
            .arg(r#"fish -ic 'eval "$APPLICATIONLAUNCHER_TERMINAL_COMMAND"; exec fish'"#)
            .env("APPLICATIONLAUNCHER_TERMINAL_COMMAND", command);
        scrub_command_env(&mut cmd);
        let _ = cmd.spawn();
    });
}

pub(crate) fn launch_fish_terminal(
    cd_target: Option<String>,
    command_after_cd: Option<&'static str>,
    terminal_title: Option<String>,
) {
    std::thread::spawn(move || {
        let title_command = if terminal_title.is_some() {
            r#"printf '\e]0;%s\a' "$APPLICATIONLAUNCHER_TERMINAL_TITLE"; "#
        } else {
            ""
        };
        let fish_command = match command_after_cd {
            Some(command) => format!(
                r#"{title_command}if test -n "$APPLICATIONLAUNCHER_CD_TARGET"; cd "$APPLICATIONLAUNCHER_CD_TARGET"; end; {command}; exec fish"#
            ),
            None => {
                format!(
                    r#"{title_command}if test -n "$APPLICATIONLAUNCHER_CD_TARGET"; cd "$APPLICATIONLAUNCHER_CD_TARGET"; end; exec fish"#
                )
            }
        };

        let mut cmd = Command::new("xfce4-terminal");
        if let Some(title) = terminal_title {
            cmd.arg("--title")
                .arg(&title)
                .env("APPLICATIONLAUNCHER_TERMINAL_TITLE", title);
        } else {
            cmd.env("APPLICATIONLAUNCHER_TERMINAL_TITLE", "");
        }
        cmd.arg("--command")
            .arg(format!("fish -ic '{}'", fish_command));

        if let Some(target) = cd_target {
            cmd.env("APPLICATIONLAUNCHER_CD_TARGET", target);
        } else {
            cmd.env("APPLICATIONLAUNCHER_CD_TARGET", "");
        }

        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("PYTHONHOME");
        cmd.env_remove("VIRTUAL_ENV");
        cmd.env_remove("UV_ACTIVE");

        let _ = cmd.spawn();
    });
}

pub(crate) fn launch_terminal_window() {
    std::thread::spawn(move || {
        let mut cmd = Command::new("xfce4-terminal");
        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("PYTHONHOME");
        cmd.env_remove("VIRTUAL_ENV");
        cmd.env_remove("UV_ACTIVE");
        let _ = cmd.spawn();
    });
}

pub(crate) fn scrub_command_env(command: &mut Command) {
    command.env_remove("PYTHONPATH");
    command.env_remove("PYTHONHOME");
    command.env_remove("VIRTUAL_ENV");
    command.env_remove("UV_ACTIVE");
}

pub(crate) fn clone_terminal_command_for_window(win: &WindowInfo) -> Option<&'static str> {
    let mut values = vec![win.title.as_str(), win.class.as_str()];
    if let Some(process) = win.active_process.as_deref() {
        values.push(process);
    }
    for entry in &win.process_chain {
        values.push(&entry.name);
    }

    let matches = |needle: &str| {
        values
            .iter()
            .any(|value| normalize_app_match_key(value).contains(needle))
    };

    if matches("codex") {
        Some("codex resume --last")
    } else if matches("agy") {
        Some("agy -c")
    } else if matches("htop") {
        Some("htop")
    } else {
        None
    }
}

pub(crate) fn source_terminal_title_for_clone(win: &WindowInfo) -> String {
    let Some(proc_name) = win.active_process.as_deref() else {
        return win.title.clone();
    };
    let proc_key = normalize_app_match_key(proc_name);
    if proc_key.is_empty() {
        return win.title.clone();
    }

    for sep in [" - ", " — ", " – ", " : ", " | "] {
        let parts: Vec<&str> = win.title.split(sep).collect();
        if parts.len() >= 3
            && parts
                .first()
                .is_some_and(|part| normalize_app_match_key(part) == proc_key)
        {
            return parts
                .iter()
                .skip(1)
                .map(|part| part.trim())
                .collect::<Vec<_>>()
                .join(sep);
        }

        if parts.len() >= 3 && normalize_app_match_key(parts[1]) == proc_key {
            let mut rebuilt = Vec::with_capacity(parts.len() - 1);
            rebuilt.push(parts[0].trim());
            rebuilt.extend(parts.iter().skip(2).map(|part| part.trim()));
            return rebuilt.join(sep);
        }
    }

    win.title.clone()
}

pub(crate) fn is_chrome_like_window(win: &WindowInfo) -> bool {
    let mut values = vec![win.class.as_str()];
    if let Some(process) = win.active_process.as_deref() {
        values.push(process);
    }
    if let Some(path) = &win.exe_path {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            values.push(name);
        }
    }

    values.iter().any(|value| {
        matches!(
            normalize_app_match_key(value).as_str(),
            "googlechrome" | "chrome" | "chromium" | "chromiumbrowser"
        )
    })
}

pub(crate) fn is_pcmanfm_window(win: &WindowInfo) -> bool {
    let mut values = vec![win.class.as_str(), win.title.as_str()];
    if let Some(process) = win.active_process.as_deref() {
        values.push(process);
    }
    if let Some(path) = &win.exe_path {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            values.push(name);
        }
    }

    values
        .iter()
        .any(|value| normalize_app_match_key(value).contains("pcmanfm"))
}

pub(crate) fn is_pcmanfm_class(class_lower: &str) -> bool {
    normalize_app_match_key(class_lower).contains("pcmanfm")
}

pub(crate) fn is_dolphin_window(win: &WindowInfo) -> bool {
    let mut values = vec![win.class.as_str(), win.title.as_str()];
    if let Some(process) = win.active_process.as_deref() {
        values.push(process);
    }
    if let Some(path) = &win.exe_path {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            values.push(name);
        }
    }

    values
        .iter()
        .any(|value| normalize_app_match_key(value).contains("dolphin"))
}

pub(crate) fn extract_url_from_text(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| token.starts_with("http://") || token.starts_with("https://"))
        .map(|token| {
            token
                .trim_matches(|c: char| matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | ';'))
                .to_string()
        })
        .filter(|url| !url.is_empty())
}

pub(crate) fn clone_chrome_window(win: &WindowInfo) -> bool {
    let Some(url) = extract_url_from_text(&win.title) else {
        return false;
    };

    let mut command = if let Some(exe) = &win.exe_path {
        Command::new(exe)
    } else {
        Command::new("google-chrome")
    };
    command.arg("--new-window").arg(url);
    command.env_remove("PYTHONPATH");
    command.env_remove("PYTHONHOME");
    command.env_remove("VIRTUAL_ENV");
    command.env_remove("UV_ACTIVE");
    command.spawn().is_ok()
}

pub(crate) fn expand_display_path_candidate(value: &str) -> Option<PathBuf> {
    let trimmed = value
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | ';'));
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        return Some(PathBuf::from(home).join(rest));
    }

    if trimmed == "~" {
        return std::env::var("HOME").ok().map(PathBuf::from);
    }

    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        return Some(path);
    }

    if trimmed.contains('/') {
        let home = std::env::var("HOME").ok()?;
        return Some(PathBuf::from(home).join(trimmed));
    }

    None
}

pub(crate) fn normalize_file_manager_target(value: &str) -> Option<String> {
    let trimmed = value
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | ';'));
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains(['\n', '\r', '\t']) || trimmed.contains("No such file or directory") {
        return None;
    }
    if trimmed.contains("://") {
        return Some(trimmed.to_string());
    }
    let path = expand_display_path_candidate(trimmed)?;
    if path.is_dir() {
        Some(path.to_string_lossy().to_string())
    } else {
        None
    }
}

pub(crate) fn accessible_location_for_window(win: &WindowInfo) -> Option<String> {
    let mut command = Command::new("python3");
    command.arg("-c").arg(ATSPI_LOCATION_PROBE);
    if let Some(pid) = win.pid {
        command.arg("--pid").arg(pid.to_string());
    }
    command.arg("--title").arg(&win.title);
    command.arg("--class").arg(&win.class);
    scrub_command_env(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout);
    let line = value.lines().map(str::trim).find(|line| !line.is_empty())?;
    normalize_file_manager_target(line)
}

pub(crate) fn pcmanfm_path_from_title(title: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    candidates.push(title.trim());

    for separator in [" — ", " – ", " - ", " : ", " | "] {
        candidates.extend(title.split(separator).map(str::trim));
    }

    candidates
        .into_iter()
        .find_map(expand_display_path_candidate)
        .filter(|path| path.is_dir())
}

pub(crate) fn pcmanfm_location_hint_from_title(title: &str) -> Option<String> {
    for part in title
        .split(['—', '–'])
        .flat_map(|part| part.split(" - "))
        .map(str::trim)
    {
        if part.is_empty()
            || part.contains(['\n', '\r', '\t'])
            || part.contains("No such file or directory")
            || normalize_app_match_key(part).contains("pcmanfm")
        {
            continue;
        }
        if expand_display_path_candidate(part).is_some() {
            continue;
        }
        if part.contains('/')
            || part.starts_with('.')
            || part.starts_with('(')
            || part.contains("://")
        {
            continue;
        }
        return Some(part.to_string());
    }

    None
}

pub(crate) fn clone_pcmanfm_with_fish_cd(target_hint: String) {
    std::thread::spawn(move || {
        let fallback_target = if normalize_app_match_key(&target_hint) == "trash" {
            "trash:///".to_string()
        } else {
            target_hint.clone()
        };
        let mut cmd = Command::new("fish");
        cmd.arg("-ic")
            .arg(
                r#"if cd "$APPLICATIONLAUNCHER_PCMANFM_TARGET"; pcmanfm --new-win "$PWD"; else; pcmanfm --new-win "$APPLICATIONLAUNCHER_PCMANFM_FALLBACK"; end"#,
            )
            .env("APPLICATIONLAUNCHER_PCMANFM_TARGET", target_hint)
            .env("APPLICATIONLAUNCHER_PCMANFM_FALLBACK", fallback_target);
        scrub_command_env(&mut cmd);
        let _ = cmd.spawn();
    });
}

pub(crate) fn launch_pcmanfm_target(exe_path: Option<PathBuf>, target: &str) -> bool {
    let mut command = if let Some(exe) = exe_path {
        Command::new(exe)
    } else {
        Command::new("pcmanfm")
    };
    command.arg("--new-win").arg(target);
    scrub_command_env(&mut command);
    command.spawn().is_ok()
}

pub(crate) fn clone_pcmanfm_window(win: &WindowInfo) -> bool {
    let win = win.clone();
    std::thread::spawn(move || {
        if let Some(target) = accessible_location_for_window(&win) {
            let _ = launch_pcmanfm_target(win.exe_path.clone(), &target);
            return;
        }

        if let Some(target) = pcmanfm_path_from_title(&win.title) {
            let _ = launch_pcmanfm_target(win.exe_path.clone(), &target.to_string_lossy());
            return;
        }

        if let Some(target_hint) = pcmanfm_location_hint_from_title(&win.title) {
            clone_pcmanfm_with_fish_cd(target_hint);
            return;
        }

        let fallback = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let _ = launch_pcmanfm_target(win.exe_path.clone(), &fallback);
    });
    true
}

pub(crate) fn launch_dolphin_target(exe_path: Option<PathBuf>, target: Option<&str>) -> bool {
    let mut command = if let Some(exe) = exe_path {
        Command::new(exe)
    } else {
        Command::new("dolphin")
    };
    command.arg("--new-window");
    if let Some(target) = target {
        command.arg(target);
    }
    scrub_command_env(&mut command);
    command.spawn().is_ok()
}

pub(crate) fn launch_dolphin_app() -> bool {
    let target = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    launch_dolphin_target(None, Some(&target))
}

pub(crate) fn clone_dolphin_window(win: &WindowInfo) -> bool {
    let win = win.clone();
    std::thread::spawn(move || {
        if let Some(target) = accessible_location_for_window(&win) {
            let _ = launch_dolphin_target(win.exe_path.clone(), Some(&target));
            return;
        }

        if let Some(target) = pcmanfm_path_from_title(&win.title) {
            let target = target.to_string_lossy().to_string();
            let _ = launch_dolphin_target(win.exe_path.clone(), Some(&target));
            return;
        }

        let fallback = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let _ = launch_dolphin_target(win.exe_path.clone(), Some(&fallback));
    });
    true
}

pub(crate) fn launch_desktop_entry(desktop_file_path: &Path) -> bool {
    let Some(desktop_id) = desktop_file_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    let desktop_id = desktop_id.to_string();
    std::thread::spawn(move || {
        let mut cmd = Command::new("gtk-launch");
        cmd.arg(&desktop_id);
        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("VIRTUAL_ENV");
        cmd.env_remove("UV_ACTIVE");
        let _ = cmd.spawn();
    });
    true
}
pub(crate) fn parse_desktop_file(path: &Path, theme: &str) -> Option<AppInfo> {
    let content = std::fs::read_to_string(path).ok()?;

    let mut in_desktop_entry = false;
    let mut name = None;
    let mut exec = None;
    let mut icon = None;
    let mut comment = None;
    let mut no_display = false;
    let mut is_application = false;
    let mut is_settings_module = false;
    let mut exec_command = None;
    let mut x_kde_alias_for = None;

    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
        let name_lower = file_name.to_lowercase();
        if name_lower.starts_with("kcm_") {
            is_settings_module = true;
        }
    }

    // Use current locale language code if available
    let lang = std::env::var("LANG")
        .ok()
        .and_then(|l| l.split('.').next().map(|s| s.to_string()))
        .and_then(|l| l.split('_').next().map(|s| s.to_string()));

    let name_key = lang.as_ref().map(|l| format!("Name[{}]", l));
    let comment_key = lang.as_ref().map(|l| format!("Comment[{}]", l));

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if line == "[Desktop Entry]" {
                in_desktop_entry = true;
            } else {
                in_desktop_entry = false;
            }
            continue;
        }

        if !in_desktop_entry {
            continue;
        }

        if let Some(pos) = line.find('=') {
            let key = line[..pos].trim();
            let val = line[pos + 1..].trim();

            if key == "Name" && name.is_none() {
                name = Some(val.to_string());
            } else if let Some(ref nk) = name_key {
                if key == nk {
                    name = Some(val.to_string());
                }
            }

            if key == "Comment" && comment.is_none() {
                comment = Some(val.to_string());
            } else if let Some(ref ck) = comment_key {
                if key == ck {
                    comment = Some(val.to_string());
                }
            }

            if key == "Exec" {
                exec = Some(val.to_string());
                exec_command = Some(val.to_string());
            }
            if key == "Icon" {
                icon = Some(val.to_string());
            }
            if key == "NoDisplay" && val.to_lowercase() == "true" {
                no_display = true;
            }
            if key == "Type" && val == "Application" {
                is_application = true;
            }
            if key == "Categories" && val.split(';').any(|c| c == "SettingsPanel") {
                is_settings_module = true;
            }
            if key == "X-KDE-AliasFor" {
                x_kde_alias_for = Some(val.to_string());
            }
        }
    }

    if x_kde_alias_for.as_deref() == Some("systemsettings") && no_display {
        is_settings_module = true;
    }

    if let Some(exec_cmd) = exec_command.as_deref() {
        let exec_lower = exec_cmd.to_lowercase();
        if exec_lower.starts_with("kcmshell")
            || exec_lower.contains(" kcm_")
            || exec_lower.starts_with("systemsettings kcm_")
        {
            is_settings_module = true;
        }
    }

    if (no_display && !is_settings_module) || !is_application {
        return None;
    }

    let name = name?;
    let exec = exec?;

    let is_terminal_app = is_terminal_app_name(&name)
        || is_terminal_app_name(&exec)
        || command_basename(&exec)
            .as_deref()
            .is_some_and(is_terminal_app_name);

    let icon_path = icon.and_then(|i| {
        let p = PathBuf::from(&i);
        if p.is_absolute() && p.exists() {
            return Some(p);
        }
        if !is_terminal_app && is_terminal_icon_name(&i) {
            return None;
        }
        lookup_theme_icon_exact(theme, &i)
    });

    Some(AppInfo {
        name,
        exec,
        icon_path,
        comment,
        desktop_file_path: path.to_path_buf(),
        is_settings_module,
    })
}

pub(crate) fn get_installed_apps(theme: &str) -> Vec<AppInfo> {
    let mut apps = Vec::new();
    let mut app_dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        app_dirs.push(PathBuf::from(format!("{}/.local/share/applications", home)));
    }
    let flatpak_dir = PathBuf::from("/var/lib/flatpak/exports/share/applications");
    if flatpak_dir.exists() {
        app_dirs.push(flatpak_dir);
    }
    let user_flatpak_dir = if let Ok(home) = std::env::var("HOME") {
        Some(PathBuf::from(format!(
            "{}/.local/share/flatpak/exports/share/applications",
            home
        )))
    } else {
        None
    };
    if let Some(dir) = user_flatpak_dir {
        if dir.exists() {
            app_dirs.push(dir);
        }
    }

    let mut seen_entries = std::collections::HashSet::new();

    for dir in app_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().map_or(false, |ext| ext == "desktop") {
                    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                        if seen_entries.contains(file_name) {
                            continue;
                        }
                        seen_entries.insert(file_name.to_string());
                    }

                    if let Some(app) = parse_desktop_file(&path, theme) {
                        apps.push(app);
                    }
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

pub(crate) fn desktop_entry_search_dirs() -> Vec<PathBuf> {
    let mut app_dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        app_dirs.push(PathBuf::from(format!("{}/.local/share/applications", home)));
        let user_flatpak_dir = PathBuf::from(format!(
            "{}/.local/share/flatpak/exports/share/applications",
            home
        ));
        if user_flatpak_dir.exists() {
            app_dirs.push(user_flatpak_dir);
        }
    }
    let flatpak_dir = PathBuf::from("/var/lib/flatpak/exports/share/applications");
    if flatpak_dir.exists() {
        app_dirs.push(flatpak_dir);
    }
    app_dirs
}

pub(crate) fn resolve_desktop_file_path(desktop_file_name: &str) -> Option<PathBuf> {
    let trimmed = desktop_file_name.trim();
    if trimmed.is_empty() {
        return None;
    }

    let candidate = PathBuf::from(trimmed);
    if candidate.is_absolute() && candidate.exists() {
        return Some(candidate);
    }

    let base_name = if trimmed.ends_with(".desktop") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.desktop")
    };

    for dir in desktop_entry_search_dirs() {
        let path = dir.join(&base_name);
        if path.exists() {
            return Some(path);
        }
    }

    None
}
