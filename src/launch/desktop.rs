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
        let raw = program_metadata::desktop::read(
            path,
            language.as_deref(),
            std::time::Instant::now() + std::time::Duration::from_millis(100),
        )
        .ok()?;
        let entry = launcher_entry(path, raw);
        if desktop_stamp(&std::fs::metadata(path).ok()?, language) != stamp {
            return None;
        }
        if self.entries.len() >= 4096 {
            self.entries.clear();
        }
        self.entries
            .insert(path.to_path_buf(), (stamp, entry.clone()));
        entry
    }
}

fn launcher_entry(path: &Path, entry: program_metadata::desktop::Entry) -> Option<DesktopEntry> {
    // KDE settings-module inclusion is launcher presentation policy, not a
    // reason for the shared raw parser to discard other invisible entries.
    let mut is_settings_module = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_lowercase().starts_with("kcm_"))
        || entry
            .categories
            .iter()
            .any(|category| category == "SettingsPanel")
        || entry.kde_alias_for.as_deref() == Some("systemsettings") && entry.no_display;
    if let Some(exec_cmd) = entry.exec.as_deref() {
        let exec_lower = exec_cmd.to_lowercase();
        if exec_lower.starts_with("kcmshell")
            || exec_lower.contains(" kcm_")
            || exec_lower.starts_with("systemsettings kcm_")
        {
            is_settings_module = true;
        }
    }

    if entry.hidden
        || (entry.no_display && !is_settings_module)
        || entry.entry_type.as_deref() != Some("Application")
    {
        return None;
    }

    Some(DesktopEntry {
        name: entry.name?,
        exec: entry.exec?,
        icon: entry.icon,
        comment: entry.comment,
        is_settings_module,
        try_exec: entry.try_exec,
        only_show_in: entry.only_show_in,
        not_show_in: entry.not_show_in,
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
    let lang = program_metadata::desktop::current_locale();
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
    // Per-scan negatives expire immediately; newly installed icons never stay hidden.
    let mut icons = std::collections::HashMap::new();

    for path in program_metadata::desktop::discover(&desktop_entry_search_dirs(), 16_384) {
        if let Some(app) = parse_desktop_file_with_icons(&path, &mut |name| {
            icons
                .entry(name.to_string())
                .or_insert_with(|| lookup_theme_icon_exact(theme, name))
                .clone()
        }) {
            apps.push(app);
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

    #[test]
    fn xdg_data_home_is_the_primary_desktop_discovery_directory() {
        const CHILD: &str = "APPLICATIONLAUNCHER_DESKTOP_XDG_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let expected =
                PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap()).join("applications");
            assert_eq!(desktop_entry_search_dirs().first(), Some(&expected));
            let found = get_installed_apps("test-theme");
            assert!(
                found
                    .iter()
                    .any(|app| app.name == "Shared desktop XDG fixture"
                        && app.desktop_file_path == expected.join("shared-xdg-fixture.desktop"))
            );
            assert_eq!(
                resolve_desktop_file_path("shared-xdg-fixture"),
                Some(expected.join("shared-xdg-fixture.desktop"))
            );
            return;
        }
        let fixture = directory();
        let apps = fixture.0.join("custom-data/applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join("shared-xdg-fixture.desktop"), "[Desktop Entry]\nType=Application\nName=Shared desktop XDG fixture\nExec=/must/not/execute\n").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "launch::desktop::optimization_tests::xdg_data_home_is_the_primary_desktop_discovery_directory", "--nocapture"])
            .env(CHILD, "1")
            .env("HOME", fixture.0.join("home"))
            .env("XDG_DATA_HOME", fixture.0.join("custom-data"))
            .env("XDG_DATA_DIRS", fixture.0.join("system-data"))
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "The isolated child must execute the assertion, not an empty test collection"
        );
    }

    #[test]
    fn shared_desktop_fields_preserve_launcher_settings_and_localization_policy() {
        let content = "[Desktop Entry]\nType=Application\nName=Settings\nName[fr]=Réglages\\sde test\nComment=Line\\nTwo\nExec=systemsettings kcm_example\nNoDisplay=true\n";
        let raw = program_metadata::desktop::parse(content, Some("fr_CA.UTF-8")).unwrap();
        let entry = launcher_entry(Path::new("/tmp/kcm_example.desktop"), raw).unwrap();
        assert_eq!(entry.name, "Réglages de test");
        assert_eq!(entry.comment.as_deref(), Some("Line\nTwo"));
        assert!(entry.is_settings_module);
        let raw = program_metadata::desktop::parse(
            "[Desktop Entry]\nType=Application\nName=Hidden\nExec=no-run\nNoDisplay=true",
            None,
        )
        .unwrap();
        assert!(launcher_entry(Path::new("/tmp/ordinary.desktop"), raw).is_none());
    }
}

pub(crate) fn desktop_entry_search_dirs() -> Vec<PathBuf> {
    program_metadata::desktop::application_dirs()
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
