use std::path::{Path, PathBuf};

use crate::is_terminal_app_name;
use crate::models::WindowIconCacheKey;

pub(crate) fn is_terminal_class(class_lower: &str) -> bool {
    class_lower.contains("terminal")
        || class_lower == "konsole"
        || class_lower == "kitty"
        || class_lower == "alacritty"
        || class_lower == "wezterm"
        || class_lower == "foot"
}

pub(crate) fn window_icon_cache_key(
    class: &str,
    desktop_file_name: Option<&str>,
    active_process: Option<&str>,
    executable: Option<&Path>,
) -> WindowIconCacheKey {
    WindowIconCacheKey {
        class: class.trim().to_lowercase(),
        desktop_file_name: desktop_file_name.map(|value| value.trim().to_lowercase()),
        active_process: active_process.map(|value| value.trim().to_lowercase()),
        executable: executable.map(Path::to_path_buf),
    }
}

pub(crate) fn resolve_window_icon(
    theme: &str,
    class: &str,
    desktop_file_name: Option<&str>,
    active_process: Option<&str>,
    executable: Option<&Path>,
) -> Option<PathBuf> {
    let terminal_window = is_terminal_class(&class.to_lowercase());
    let tor_browser_window = is_tor_browser_identity(class);
    if terminal_window
        && let Some(child_icon) = active_process
            .and_then(|process| find_terminal_child_icon(theme, process, &application_dirs()))
    {
        return Some(child_icon);
    }

    let mut candidates = Vec::new();
    let mut push_candidate = |candidate: Option<&str>| {
        let Some(candidate) = candidate.map(str::trim).filter(|value| !value.is_empty()) else {
            return;
        };
        if !terminal_window && is_terminal_app_name(candidate) {
            return;
        }
        if !candidates
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(candidate))
        {
            candidates.push(candidate.to_string());
        }
    };

    push_candidate(desktop_file_name);
    if let Some(desktop_stem) = desktop_file_name.and_then(|value| value.strip_suffix(".desktop")) {
        push_candidate(Some(desktop_stem));
    }
    push_candidate(Some(class));
    // Terminal children are handled above only when a matching desktop entry
    // explicitly declares Terminal=true. Unknown and detached helper processes
    // retain the terminal application's icon.
    if !terminal_window {
        push_candidate(active_process);
    }
    if !tor_browser_window {
        push_candidate(
            executable
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
        );
    }

    candidates
        .into_iter()
        .find_map(|candidate| find_icon(theme, &candidate))
}

#[derive(Default)]
struct DesktopEntryMetadata {
    icon: Option<String>,
    runs_in_terminal: bool,
}

fn application_dirs() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        directories.push(PathBuf::from(data_home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        directories.push(PathBuf::from(home).join(".local/share/applications"));
    }
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|dirs| std::env::split_paths(&dirs).collect::<Vec<_>>())
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        });
    for directory in data_dirs {
        let directory = directory.join("applications");
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    directories
}

fn find_terminal_child_icon(
    theme: &str,
    active_process: &str,
    application_dirs: &[PathBuf],
) -> Option<PathBuf> {
    let process_name = Path::new(active_process.trim())
        .file_name()
        .and_then(|name| name.to_str())?
        .trim_end_matches(".desktop");
    let mut desktop_stems = vec![process_name.to_string()];
    let lowercase = process_name.to_lowercase();
    if lowercase != process_name {
        desktop_stems.push(lowercase);
    }

    for stem in &desktop_stems {
        for directory in application_dirs {
            let Some(metadata) = parse_desktop_entry(&directory.join(format!("{stem}.desktop")))
            else {
                continue;
            };
            if !metadata.runs_in_terminal {
                return None;
            }
            let Some(icon) = metadata.icon else {
                return None;
            };
            let icon_path = PathBuf::from(&icon);
            if icon_path.is_absolute() {
                return icon_path.is_file().then_some(icon_path);
            }
            return lookup_theme_icon_exact(theme, &icon);
        }
    }
    None
}

fn parse_desktop_entry(path: &Path) -> Option<DesktopEntryMetadata> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(parse_desktop_entry_content(&content))
}

fn parse_desktop_entry_content(content: &str) -> DesktopEntryMetadata {
    let mut metadata = DesktopEntryMetadata::default();
    let mut in_desktop_entry = false;
    for line in content.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_desktop_entry || line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("Icon=") {
            metadata.icon = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Terminal=") {
            metadata.runs_in_terminal = value.trim().eq_ignore_ascii_case("true");
        }
    }
    metadata
}

fn parse_icon_from_desktop(path: &Path) -> Option<String> {
    parse_desktop_entry(path)?.icon
}

pub(crate) fn lookup_theme_icon_exact(theme: &str, name: &str) -> Option<PathBuf> {
    let themes_to_check = if theme == "breeze-dark" {
        vec!["breeze-dark", "breeze", "hicolor"]
    } else if theme == "breeze" {
        vec!["breeze", "breeze-dark", "hicolor"]
    } else {
        vec![theme, "breeze-dark", "breeze", "hicolor"]
    };

    for t in themes_to_check {
        if let Some(path) = freedesktop_icons::lookup(name)
            .with_theme(t)
            .with_size(48)
            .find()
        {
            return Some(path);
        }
    }

    let pixmap = PathBuf::from(format!("/usr/share/pixmaps/{}.png", name));
    pixmap.exists().then_some(pixmap)
}

pub(crate) fn is_tor_browser_identity(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.contains("tor browser") || lower.contains("tor-browser") || lower.contains("torbrowser")
}

pub(crate) fn find_icon(theme: &str, class: &str) -> Option<PathBuf> {
    if class.is_empty() {
        return None;
    }

    let lower = class.to_lowercase();
    let mut names = vec![lower.clone(), class.to_string()];
    let is_tor_browser = is_tor_browser_identity(&lower);
    if is_tor_browser {
        names.insert(0, "org.torproject.torbrowser-launcher".to_string());
        names.push("tor-browser".to_string());
        names.push("tor-browser-alpha".to_string());
        names.push("torbrowser".to_string());
    }

    // Handle reverse-DNS formats (e.g., org.xfce.mousepad -> mousepad)
    if lower.contains('.') {
        if let Some(last) = lower.split('.').last() {
            names.push(last.to_string());
        }
    }

    // Try finding the .desktop file to see if it has a hardcoded icon path or an override name
    let app_dirs = application_dirs();

    let mut overrides = Vec::new();
    for dir in &app_dirs {
        for name in &names {
            let desktop_path = dir.join(format!("{}.desktop", name));
            if desktop_path.exists() {
                if let Some(icon_val) = parse_icon_from_desktop(&desktop_path) {
                    let p = PathBuf::from(&icon_val);
                    if p.is_absolute() && p.exists() {
                        return Some(p);
                    }
                    if !names.contains(&icon_val) && !overrides.contains(&icon_val) {
                        overrides.push(icon_val);
                    }
                }
            }
        }
    }

    // Insert overrides at the front of the names vector (highest specificity)
    for ovr in overrides.into_iter().rev() {
        names.insert(0, ovr);
    }

    // Keyword fallbacks for generic application categories
    if lower.contains("terminal") {
        names.push("utilities-terminal".to_string());
        names.push("terminal".to_string());
    }
    if lower.contains("mousepad") || lower.contains("editor") || lower.contains("text") {
        names.push("accessories-text-editor".to_string());
        names.push("mousepad".to_string());
    }
    if lower.contains("file-manager")
        || lower.contains("pcmanfm")
        || lower.contains("thunar")
        || lower.contains("dolphin")
    {
        names.push("system-file-manager".to_string());
        names.push("folder-open".to_string());
    }
    if !is_tor_browser
        && (lower.contains("web") || lower.contains("browser") || lower.contains("firefox"))
    {
        names.push("web-browser".to_string());
    }
    if lower.contains("copyq") {
        names.push("copyq".to_string());
        names.push("edit-paste".to_string());
    }

    // Try finding in specified theme and standard fallbacks
    let themes_to_check = if theme == "breeze-dark" {
        vec!["breeze-dark", "breeze", "hicolor"]
    } else if theme == "breeze" {
        vec!["breeze", "breeze-dark", "hicolor"]
    } else {
        vec![theme, "breeze-dark", "breeze", "hicolor"]
    };

    for t in themes_to_check {
        for name in &names {
            if let Some(path) = freedesktop_icons::lookup(name)
                .with_theme(t)
                .with_size(48)
                .find()
            {
                return Some(path);
            }
        }
    }

    // Look in legacy /usr/share/pixmaps as a final fallback
    for name in &names {
        let pixmap = PathBuf::from(format!("/usr/share/pixmaps/{}.png", name));
        if pixmap.exists() {
            return Some(pixmap);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_parser_uses_only_the_main_section() {
        let metadata = parse_desktop_entry_content(
            "[Desktop Entry]\nIcon=htop\nTerminal=true\n\n[Desktop Action Unsafe]\nTerminal=false\nIcon=electron\n",
        );
        assert_eq!(metadata.icon.as_deref(), Some("htop"));
        assert!(metadata.runs_in_terminal);
    }

    #[test]
    fn terminal_child_icon_requires_a_terminal_desktop_entry() {
        let root = std::env::temp_dir().join(format!(
            "applicationlauncher-terminal-icons-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let htop_icon = root.join("htop.svg");
        let electron_icon = root.join("electron.svg");
        std::fs::write(&htop_icon, "<svg/>").unwrap();
        std::fs::write(&electron_icon, "<svg/>").unwrap();
        std::fs::write(
            root.join("htop.desktop"),
            format!(
                "[Desktop Entry]\nType=Application\nTerminal=true\nIcon={}\n",
                htop_icon.display()
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("electron.desktop"),
            format!(
                "[Desktop Entry]\nType=Application\nTerminal=false\nIcon={}\n",
                electron_icon.display()
            ),
        )
        .unwrap();

        assert_eq!(
            find_terminal_child_icon("breeze", "htop", std::slice::from_ref(&root)),
            Some(htop_icon)
        );
        assert_eq!(
            find_terminal_child_icon("breeze", "electron", std::slice::from_ref(&root)),
            None
        );
        let override_root = root.join("overrides");
        std::fs::create_dir(&override_root).unwrap();
        std::fs::write(
            override_root.join("htop.desktop"),
            "[Desktop Entry]\nType=Application\nTerminal=false\nIcon=htop\n",
        )
        .unwrap();
        assert_eq!(
            find_terminal_child_icon("breeze", "htop", &[override_root, root.clone()]),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn installed_terminal_monitor_icons_resolve_when_available() {
        let directories = application_dirs();
        for process in ["htop", "nvtop"] {
            let desktop_installed = directories
                .iter()
                .any(|directory| directory.join(format!("{process}.desktop")).is_file());
            if desktop_installed {
                assert!(
                    find_terminal_child_icon("breeze-dark", process, &directories).is_some(),
                    "installed {process} desktop entry did not resolve an icon"
                );
            }
        }
    }
}
