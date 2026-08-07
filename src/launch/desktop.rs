use crate::models::AppInfo;
use crate::*;
use std::path::{Path, PathBuf};
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
