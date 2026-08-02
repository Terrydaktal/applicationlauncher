use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{HistoryEntry, RestoreReport, RestoreSpec, SnapshotDetail, TrackedWindow, app_key};

pub fn restore_snapshot(snapshot: &SnapshotDetail, current: &[TrackedWindow]) -> RestoreReport {
    restore_specs(&snapshot.windows, current)
}

pub fn restore_entries(entries: &[HistoryEntry], current: &[TrackedWindow]) -> RestoreReport {
    let specs = entries
        .iter()
        .map(|entry| (entry.window.clone(), entry.restore.clone()))
        .collect::<Vec<_>>();
    restore_specs(&specs, current)
}

pub fn reopen_entry(entry: &HistoryEntry) -> RestoreReport {
    let mut report = RestoreReport::default();
    match launch(&entry.restore) {
        Ok(()) => report.launched = 1,
        Err(err) => report
            .failures
            .push(format!("{}: {err}", entry.window.title)),
    }
    report
}

fn restore_specs(
    specs: &[(TrackedWindow, RestoreSpec)],
    current: &[TrackedWindow],
) -> RestoreReport {
    let mut report = RestoreReport::default();
    let mut used = HashSet::new();
    for (wanted, restore) in specs {
        if let Some(existing) = find_matching_window(wanted, restore, current, &used) {
            used.insert(existing.id.clone());
            apply_layout(existing, wanted, &mut report.failures);
            report.matched += 1;
            continue;
        }
        match launch(restore) {
            Ok(()) => report.launched += 1,
            Err(err) => report.failures.push(format!("{}: {err}", wanted.title)),
        }
    }
    report
}

pub(crate) fn apply_matching_layouts(
    specs: &[(TrackedWindow, RestoreSpec)],
    current: &[TrackedWindow],
    replay_activation_order: bool,
) -> (usize, Vec<String>) {
    let mut failures = Vec::new();
    let mut used = HashSet::new();
    let mut matched = Vec::new();
    for (wanted, restore) in specs {
        if let Some(existing) = find_matching_window(wanted, restore, current, &used) {
            used.insert(existing.id.clone());
            apply_layout(existing, wanted, &mut failures);
            matched.push(existing.id.clone());
        }
    }
    if replay_activation_order {
        for id in &matched {
            if !Command::new("kdotool")
                .args(["windowactivate", id])
                .status()
                .is_ok_and(|status| status.success())
            {
                failures.push(format!("Could not replay activation order for {id}"));
            }
        }
    }
    (matched.len(), failures)
}

pub(crate) fn matching_window_count(
    specs: &[(TrackedWindow, RestoreSpec)],
    current: &[TrackedWindow],
) -> usize {
    let mut used = HashSet::new();
    specs
        .iter()
        .filter(|(wanted, restore)| {
            find_matching_window(wanted, restore, current, &used)
                .is_some_and(|window| used.insert(window.id.clone()))
        })
        .count()
}

fn find_matching_window<'a>(
    wanted: &TrackedWindow,
    restore: &RestoreSpec,
    current: &'a [TrackedWindow],
    used: &HashSet<String>,
) -> Option<&'a TrackedWindow> {
    let same_app =
        |window: &&TrackedWindow| !used.contains(&window.id) && app_key(wanted) == app_key(window);

    if restore.terminal_kind.is_some() {
        return current.iter().filter(same_app).find(|window| {
            let current_restore = super::infer_restore_spec(window);
            restore.terminal_kind == current_restore.terminal_kind
                && restore.cwd == current_restore.cwd
        });
    }

    current
        .iter()
        .filter(same_app)
        .find(|window| wanted.title == window.title)
        .or_else(|| current.iter().filter(same_app).next())
}

fn apply_layout(current: &TrackedWindow, wanted: &TrackedWindow, failures: &mut Vec<String>) {
    let id = current.id.as_str();
    let run = |args: &[String]| {
        Command::new("kdotool")
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
    if wanted.desktop > 0 {
        let args = vec![
            "set_desktop_for_window".into(),
            id.into(),
            wanted.desktop.to_string(),
        ];
        if !run(&args) {
            failures.push(format!(
                "Could not move {} to desktop {}",
                wanted.title, wanted.desktop
            ));
        }
    }
    if wanted.width > 0 && wanted.height > 0 {
        let size = vec![
            "windowsize".into(),
            id.into(),
            wanted.width.to_string(),
            wanted.height.to_string(),
        ];
        let position = vec![
            "windowmove".into(),
            id.into(),
            wanted.x.to_string(),
            wanted.y.to_string(),
        ];
        if !run(&size) || !run(&position) {
            failures.push(format!("Could not restore geometry for {}", wanted.title));
        }
    }
    let property = if wanted.fullscreen {
        Some("fullscreen")
    } else if wanted.maximized {
        Some("maximized")
    } else if wanted.minimized {
        Some("minimized")
    } else {
        None
    };
    if let Some(property) = property {
        let args = vec![
            "windowstate".into(),
            "--add".into(),
            property.into(),
            id.into(),
        ];
        if !run(&args) {
            failures.push(format!(
                "Could not restore {property} state for {}",
                wanted.title
            ));
        }
    }
}

fn launch(restore: &RestoreSpec) -> Result<(), String> {
    if let Some(kind) = restore.terminal_kind.as_deref() {
        return launch_terminal(kind, restore.cwd.as_deref());
    }
    let key = restore.app_key.to_lowercase();
    if key.contains("dolphin") {
        let mut command = Command::new("dolphin");
        command.arg("--new-window");
        if let Some(cwd) = restore.cwd.as_deref() {
            command.arg(expand_home(cwd));
        }
        return command.spawn().map(|_| ()).map_err(|err| err.to_string());
    }
    if key.contains("pcmanfm") {
        let mut command = Command::new("pcmanfm");
        command.arg("--new-win");
        if let Some(cwd) = restore.cwd.as_deref() {
            command.arg(expand_home(cwd));
        }
        return command.spawn().map(|_| ()).map_err(|err| err.to_string());
    }
    let desktop = resolve_desktop_file(restore)?;
    let status = Command::new("gio")
        .arg("launch")
        .arg(&desktop)
        .status()
        .map_err(|err| format!("Could not run gio for {}: {err}", desktop.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "gio could not launch {} (exit status {status})",
            desktop.display()
        ))
    }
}

fn resolve_desktop_file(restore: &RestoreSpec) -> Result<PathBuf, String> {
    let mut data_dirs = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
                .join(".local/share")
        });
    data_dirs.push(data_home);
    data_dirs.extend(
        std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".into())
            .split(':')
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    );
    resolve_desktop_file_in(restore, &data_dirs)
}

fn resolve_desktop_file_in(
    restore: &RestoreSpec,
    data_dirs: &[PathBuf],
) -> Result<PathBuf, String> {
    let candidates = [restore.desktop_file.as_deref(), Some(&restore.app_key)];
    for candidate in candidates.into_iter().flatten() {
        let candidate = Path::new(candidate)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(candidate)
            .trim();
        if candidate.is_empty() {
            continue;
        }
        let file_names = if candidate.ends_with(".desktop") {
            vec![candidate.to_string(), format!("{candidate}.desktop")]
        } else {
            vec![format!("{candidate}.desktop")]
        };
        for file_name in file_names {
            if data_dirs
                .iter()
                .any(|dir| dir.join("applications").join(&file_name).is_file())
            {
                return Ok(data_dirs
                    .iter()
                    .map(|dir| dir.join("applications").join(&file_name))
                    .find(|path| path.is_file())
                    .expect("desktop file existence was checked"));
            }
        }
    }
    Err(format!(
        "No installed desktop file was found for {}",
        restore.app_key
    ))
}

fn launch_terminal(kind: &str, cwd: Option<&str>) -> Result<(), String> {
    let cwd = cwd
        .map(expand_home)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())));
    let command = match kind {
        "codex" => "codex resume --last; exec fish",
        "agy" => "agy -c; exec fish",
        "htop" => "htop; exec fish",
        "nvtop" => "nvtop; exec fish",
        _ => "exec fish",
    };
    Command::new("xfce4-terminal")
        .arg("--working-directory")
        .arg(cwd)
        .arg("--command")
        .arg(format!("fish -lc '{command}'"))
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())).join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_desktop_ids_that_already_end_in_desktop() {
        let root = std::env::temp_dir().join(format!(
            "applicationlauncher-desktop-id-{}",
            std::process::id()
        ));
        let applications = root.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        std::fs::write(
            applications.join("org.telegram.desktop.desktop"),
            "[Desktop Entry]\nName=Telegram\n",
        )
        .unwrap();
        let restore = RestoreSpec {
            app_key: "org.telegram".into(),
            desktop_file: Some("org.telegram.desktop".into()),
            ..Default::default()
        };

        assert_eq!(
            resolve_desktop_file_in(&restore, std::slice::from_ref(&root)).unwrap(),
            applications.join("org.telegram.desktop.desktop")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
