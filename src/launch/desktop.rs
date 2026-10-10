use crate::models::AppInfo;
use crate::*;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone)]
struct DesktopEntry {
    name: String,
    exec: String,
    icon: Option<String>,
    comment: Option<String>,
    is_settings_module: bool,
    try_exec: Option<String>,
    only_show_in: Vec<String>,
    not_show_in: Vec<String>,
}

#[derive(PartialEq, Eq)]
struct DesktopStamp {
    identity: (u64, u64, u64, i64, i64, i64, i64),
    language: Option<String>,
}

fn desktop_stamp(metadata: &std::fs::Metadata, language: Option<String>) -> DesktopStamp {
    DesktopStamp {
        identity: (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ),
        language,
    }
}

#[derive(Default)]
struct DesktopCache {
    entries: std::collections::HashMap<PathBuf, (DesktopStamp, Option<DesktopEntry>)>,
}

impl DesktopCache {
    fn read(&mut self, path: &Path, language: Option<String>) -> Option<DesktopEntry> {
        let stamp = desktop_stamp(&std::fs::metadata(path).ok()?, language.clone());
        if let Some((previous, entry)) = self.entries.get(path)
            && *previous == stamp
        {
            return entry.clone();
        }
        let content = std::fs::read_to_string(path).ok()?;
        let entry = parse_desktop_content(path, &content, language);
        if self.entries.len() >= 4096 {
            self.entries.clear();
        }
        self.entries
            .insert(path.to_path_buf(), (stamp, entry.clone()));
        entry
    }
}

fn parse_desktop_content(path: &Path, content: &str, lang: Option<String>) -> Option<DesktopEntry> {
    let mut in_desktop_entry = false;
    let mut name = None;
    let mut exec = None;
    let mut icon = None;
    let mut comment = None;
    let mut no_display = false;
    let mut hidden = false;
    let mut is_application = false;
    let mut is_settings_module = false;
    let mut exec_command = None;
    let mut x_kde_alias_for = None;
    let mut try_exec = None;
    let mut only_show_in = Vec::new();
    let mut not_show_in = Vec::new();

    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
        let name_lower = file_name.to_lowercase();
        if name_lower.starts_with("kcm_") {
            is_settings_module = true;
        }
    }

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
            if key == "Hidden" && val.eq_ignore_ascii_case("true") {
                hidden = true;
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
            if key == "TryExec" {
                try_exec = Some(val.to_string());
            }
            if key == "OnlyShowIn" {
                only_show_in = val
                    .split(';')
                    .filter(|desktop| !desktop.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
            }
            if key == "NotShowIn" {
                not_show_in = val
                    .split(';')
                    .filter(|desktop| !desktop.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
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

    if hidden || (no_display && !is_settings_module) || !is_application {
        return None;
    }

    Some(DesktopEntry {
        name: name?,
        exec: exec?,
        icon,
        comment,
        is_settings_module,
        try_exec,
        only_show_in,
        not_show_in,
    })
}

pub(crate) fn parse_desktop_file(path: &Path, theme: &str) -> Option<AppInfo> {
    parse_desktop_file_with_icons(path, &mut |name| lookup_theme_icon_exact(theme, name))
}

fn parse_desktop_file_with_icons(
    path: &Path,
    icon_lookup: &mut impl FnMut(&str) -> Option<PathBuf>,
) -> Option<AppInfo> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<DesktopCache>> = std::sync::OnceLock::new();
    let lang = std::env::var("LANG")
        .ok()
        .and_then(|l| l.split('.').next().map(str::to_owned))
        .and_then(|l| l.split('_').next().map(str::to_owned));
    let entry = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .read(path, lang)?;
    let DesktopEntry {
        name,
        exec,
        icon,
        comment,
        is_settings_module,
        try_exec,
        only_show_in,
        not_show_in,
    } = entry;
    let current_desktop_value = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let current_desktops = current_desktop_value
        .split(':')
        .flat_map(|desktop| desktop.split(';'))
        .filter(|desktop| !desktop.is_empty())
        .collect::<Vec<_>>();
    if !only_show_in.is_empty()
        && !only_show_in.iter().any(|desktop| {
            current_desktops
                .iter()
                .any(|current| current.eq_ignore_ascii_case(desktop))
        })
    {
        return None;
    }
    if not_show_in.iter().any(|desktop| {
        current_desktops
            .iter()
            .any(|current| current.eq_ignore_ascii_case(desktop))
    }) {
        return None;
    }

    if let Some(try_exec) = try_exec.as_deref().filter(|value| !value.trim().is_empty())
        && !executable_is_available(try_exec)
    {
        return None;
    }

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
        icon_lookup(&i)
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
    let mut app_dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        app_dirs.push(PathBuf::from(format!("{}/.local/share/applications", home)));
    }
    app_dirs.extend(
        xdg_data_dirs()
            .into_iter()
            .map(|dir| dir.join("applications")),
    );
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
    // Per-scan negatives expire immediately; newly installed icons never stay hidden.
    let mut icons = std::collections::HashMap::new();

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

                    if let Some(app) = parse_desktop_file_with_icons(&path, &mut |name| {
                        icons
                            .entry(name.to_string())
                            .or_insert_with(|| lookup_theme_icon_exact(theme, name))
                            .clone()
                    }) {
                        apps.push(app);
                    }
                }
            }
        }
    }

    apps.sort_by_cached_key(|app| app.name.to_lowercase());
    apps
}

#[cfg(test)]
mod optimization_tests {
    use super::*;
    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn directory() -> Directory {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = Directory(
            std::env::temp_dir().join(format!("al-desktop-cache-{}-{nonce}", std::process::id())),
        );
        std::fs::create_dir(&directory.0).unwrap();
        directory
    }
    #[test]
    fn desktop_cache_reuses_parsing_and_invalidates_edits_locale_deletion_and_negatives() {
        let directory = directory();
        let path = directory.0.join("app.desktop");
        std::fs::write(
            &path,
            "[Desktop Entry]\nType=Application\nName=App\nName[fr]=Nom\nExec=example\n",
        )
        .unwrap();
        let mut cache = DesktopCache::default();
        assert_eq!(cache.read(&path, None).unwrap().name, "App");
        let allocation = cache.entries[&path].1.as_ref().unwrap().name.as_ptr();
        for _ in 0..100 {
            assert_eq!(cache.read(&path, None).unwrap().name, "App");
            assert_eq!(
                cache.entries[&path].1.as_ref().unwrap().name.as_ptr(),
                allocation
            );
        }
        assert_eq!(cache.read(&path, Some("fr".into())).unwrap().name, "Nom");
        std::fs::write(
            &path,
            "[Desktop Entry]\nType=Application\nName=Hidden\nHidden=true\nExec=example\n",
        )
        .unwrap();
        assert!(cache.read(&path, None).is_none());
        assert!(cache.read(&path, None).is_none());
        std::fs::write(
            &path,
            "[Desktop Entry]\nType=Application\nName=Installed\nExec=example\n",
        )
        .unwrap();
        assert_eq!(cache.read(&path, None).unwrap().name, "Installed");
        std::fs::remove_file(&path).unwrap();
        assert!(cache.read(&path, None).is_none());
    }
    #[test]
    fn cached_desktop_entry_rechecks_try_exec_and_icon_availability() {
        use std::os::unix::fs::PermissionsExt;
        let directory = directory();
        let path = directory.0.join("app.desktop");
        let program = directory.0.join("program");
        std::fs::write(&path, format!("[Desktop Entry]\nType=Application\nName=App\nExec={}\nTryExec={}\nIcon=test-icon\n", program.display(), program.display())).unwrap();
        assert!(parse_desktop_file(&path, "test-theme").is_none());
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let without = parse_desktop_file_with_icons(&path, &mut |_| None).unwrap();
        assert!(without.icon_path.is_none());
        let icon = directory.0.join("test.svg");
        std::fs::write(&icon, "test icon").unwrap();
        let with = parse_desktop_file_with_icons(&path, &mut |_| Some(icon.clone())).unwrap();
        assert_eq!(with.icon_path, Some(icon));
        std::fs::remove_file(&program).unwrap();
        assert!(parse_desktop_file_with_icons(&path, &mut |_| None).is_none());
    }
}

pub(crate) fn desktop_entry_search_dirs() -> Vec<PathBuf> {
    let mut app_dirs = Vec::new();
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
    app_dirs.extend(
        xdg_data_dirs()
            .into_iter()
            .map(|dir| dir.join("applications")),
    );
    app_dirs
}

fn xdg_data_dirs() -> Vec<PathBuf> {
    std::env::var_os("XDG_DATA_DIRS")
        .map(|dirs| std::env::split_paths(&dirs).collect())
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        })
}

fn executable_is_available(value: &str) -> bool {
    let command = value.split_whitespace().next().unwrap_or_default();
    let path = Path::new(command);
    if path.is_absolute() {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(command))
        .any(|candidate| candidate.is_file())
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
