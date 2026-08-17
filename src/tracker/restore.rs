use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum LaunchUnavailableCode {
    UnsupportedTerminalKind,
    MissingExecutable,
    MissingDesktopEntry,
    NonLaunchableDesktopEntry,
}

#[derive(Debug)]
struct LaunchUnavailable {
    code: LaunchUnavailableCode,
    detail: String,
}

impl fmt::Display for LaunchUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

struct LaunchPlan {
    program: PathBuf,
    arguments: Vec<OsString>,
    description: String,
}

impl LaunchPlan {
    fn run(self) -> Result<(), String> {
        let mut command = Command::new(&self.program);
        command.args(self.arguments);
        crate::process::spawn_and_reap(command).map_err(|err| {
            format!(
                "Could not launch {} with {}: {err}",
                self.description,
                self.program.display()
            )
        })
    }
}

pub(crate) fn unavailable_reason(restore: &RestoreSpec) -> Option<LaunchUnavailableCode> {
    launch_plan(restore).err().map(|error| error.code)
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
            apply_layout(existing, wanted, false, &mut report.failures);
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
            apply_layout(existing, wanted, replay_activation_order, &mut failures);
            matched.push(existing.id.clone());
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

    let app_matches = current.iter().filter(same_app).collect::<Vec<_>>();
    app_matches
        .iter()
        .find(|window| wanted.title == window.title)
        .copied()
        .or_else(|| (app_matches.len() == 1).then(|| app_matches[0]))
}

fn apply_layout(
    current: &TrackedWindow,
    wanted: &TrackedWindow,
    activate: bool,
    failures: &mut Vec<String>,
) {
    let id = current.id.as_str();
    let mut args = Vec::new();
    if wanted.on_all_desktops {
        args.extend(["set_desktop_for_window".into(), id.into(), "all".into()]);
    } else if wanted.desktop > 0 {
        args.extend([
            "set_desktop_for_window".into(),
            id.into(),
            wanted.desktop.to_string(),
        ]);
    }
    if wanted.width > 0 && wanted.height > 0 {
        args.extend([
            "windowsize".into(),
            id.into(),
            wanted.width.to_string(),
            wanted.height.to_string(),
        ]);
        args.extend([
            "windowmove".into(),
            id.into(),
            wanted.x.to_string(),
            wanted.y.to_string(),
        ]);
    }
    let mut state_args = vec!["windowstate".into()];
    for property in [
        "fullscreen",
        "maximized",
        "maximized_horz",
        "maximized_vert",
        "minimized",
    ] {
        state_args.extend(["--remove".into(), property.into()]);
    }
    args.extend(state_args_with_window(state_args, id));

    let mut state_args = vec!["windowstate".into()];
    if wanted.fullscreen {
        state_args.extend(["--add".into(), "fullscreen".into()]);
    } else if wanted.maximized {
        state_args.extend([
            "--add".into(),
            "maximized_horz".into(),
            "--add".into(),
            "maximized_vert".into(),
        ]);
    } else if wanted.minimized {
        state_args.extend(["--add".into(), "minimized".into()]);
    }
    if state_args.len() > 1 {
        args.extend(state_args_with_window(state_args, id));
    }
    if activate {
        args.extend(activation_args(id));
    }

    let mut command = Command::new(crate::process::kdotool_path());
    command.args(&args);
    if !crate::process::status_with_timeout(command, Duration::from_secs(3))
        .is_ok_and(|status| status.success())
    {
        failures.push(format!("Could not restore layout for {}", wanted.title));
        if activate && let Err(err) = activate_and_raise(id) {
            failures.push(format!("Could not activate restored window {id}: {err}"));
        }
    }
}

fn activate_and_raise(id: &str) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 0..3 {
        let mut command = Command::new(crate::process::kdotool_path());
        command.args(activation_args(id));
        match crate::process::output_with_timeout(command, Duration::from_secs(2)) {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => {
                last_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if last_error.is_empty() {
                    last_error = output.status.to_string();
                }
            }
            Err(err) => last_error = err.to_string(),
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Err(last_error)
}

fn activation_args(id: &str) -> Vec<String> {
    vec![
        "windowactivate".into(),
        id.into(),
        "windowraise".into(),
        id.into(),
    ]
}

fn state_args_with_window(mut args: Vec<String>, id: &str) -> Vec<String> {
    args.push(id.to_string());
    args
}

fn launch(restore: &RestoreSpec) -> Result<(), String> {
    launch_plan(restore)
        .map_err(|error| error.to_string())?
        .run()
}

fn launch_plan(restore: &RestoreSpec) -> Result<LaunchPlan, LaunchUnavailable> {
    if let Some(kind) = restore.terminal_kind.as_deref() {
        return terminal_launch_plan(kind, restore.cwd.as_deref());
    }
    let key = restore.app_key.to_lowercase();
    if key.contains("dolphin")
        && let Some(program) = crate::process::executable_path("dolphin")
    {
        let mut arguments = vec![OsString::from("--new-window")];
        if let Some(cwd) = restore.cwd.as_deref() {
            arguments.push(expand_home(cwd).into_os_string());
        }
        return Ok(LaunchPlan {
            program,
            arguments,
            description: "Dolphin window".into(),
        });
    }
    if key.contains("pcmanfm")
        && let Some(program) = crate::process::executable_path("pcmanfm")
    {
        let mut arguments = vec![OsString::from("--new-win")];
        if let Some(cwd) = restore.cwd.as_deref() {
            arguments.push(expand_home(cwd).into_os_string());
        }
        return Ok(LaunchPlan {
            program,
            arguments,
            description: "PCManFM window".into(),
        });
    }
    let desktop = resolve_desktop_file(restore)?;
    validate_desktop_file(&desktop)?;
    let program = required_executable("gio")?;
    Ok(LaunchPlan {
        program,
        arguments: vec![OsString::from("launch"), desktop.clone().into_os_string()],
        description: format!("desktop entry {}", desktop.display()),
    })
}

fn validate_desktop_file(path: &Path) -> Result<(), LaunchUnavailable> {
    let contents = std::fs::read_to_string(path).map_err(|err| LaunchUnavailable {
        code: LaunchUnavailableCode::NonLaunchableDesktopEntry,
        detail: format!("Could not read desktop entry {}: {err}", path.display()),
    })?;
    let mut in_desktop_entry = false;
    let mut application_type = false;
    let mut hidden = false;
    let mut no_display = false;
    let mut has_exec = false;
    let mut dbus_activatable = false;
    let mut try_exec = None;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Type" => application_type = value.trim() == "Application",
            "Hidden" => hidden = value.trim().eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.trim().eq_ignore_ascii_case("true"),
            "Exec" => has_exec = !value.trim().is_empty(),
            "TryExec" => try_exec = Some(value.trim().to_string()),
            "DBusActivatable" => {
                dbus_activatable = value.trim().eq_ignore_ascii_case("true");
            }
            _ => {}
        }
    }
    if let Some(try_exec) = try_exec.filter(|value| !value.is_empty())
        && crate::process::executable_path(&try_exec).is_none()
    {
        return Err(LaunchUnavailable {
            code: LaunchUnavailableCode::MissingExecutable,
            detail: format!(
                "Desktop entry {} requires unavailable executable {try_exec}",
                path.display()
            ),
        });
    }
    if application_type && !hidden && !no_display && (has_exec || dbus_activatable) {
        return Ok(());
    }
    Err(LaunchUnavailable {
        code: LaunchUnavailableCode::NonLaunchableDesktopEntry,
        detail: format!(
            "Desktop entry {} is hidden or has no launchable application action",
            path.display()
        ),
    })
}

fn required_executable(program: &str) -> Result<PathBuf, LaunchUnavailable> {
    crate::process::executable_path(program).ok_or_else(|| LaunchUnavailable {
        code: LaunchUnavailableCode::MissingExecutable,
        detail: format!("Required executable {program} is unavailable"),
    })
}

fn resolve_desktop_file(restore: &RestoreSpec) -> Result<PathBuf, LaunchUnavailable> {
    let mut data_dirs = Vec::new();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
                .join(".local/share")
        });
    data_dirs.push(data_home.clone());
    data_dirs.push(data_home.join("flatpak/exports/share"));
    data_dirs.extend(
        std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".into())
            .split(':')
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    );
    data_dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    resolve_desktop_file_in(restore, &data_dirs)
}

fn resolve_desktop_file_in(
    restore: &RestoreSpec,
    data_dirs: &[PathBuf],
) -> Result<PathBuf, LaunchUnavailable> {
    let candidates = [restore.desktop_file.as_deref(), Some(&restore.app_key)];
    for candidate in candidates.into_iter().flatten() {
        let candidate_path = expand_home(candidate);
        if candidate_path.is_file() && candidate_path.components().count() > 1 {
            return Ok(candidate_path);
        }
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
    Err(LaunchUnavailable {
        code: LaunchUnavailableCode::MissingDesktopEntry,
        detail: format!(
            "No installed desktop file was found for {}",
            restore.app_key
        ),
    })
}

fn terminal_launch_plan(kind: &str, cwd: Option<&str>) -> Result<LaunchPlan, LaunchUnavailable> {
    let command = terminal_shell_command(kind)?;
    let program = required_executable("xfce4-terminal")?;
    required_executable("fish")?;
    if let Some((executable, _)) = command {
        required_executable(executable)?;
        required_executable("bash")?;
    }
    let cwd = cwd
        .map(expand_home)
        .filter(|path| path.is_dir())
        .unwrap_or_else(home_directory);
    Ok(LaunchPlan {
        program,
        arguments: vec![
            OsString::from("--working-directory"),
            cwd.into_os_string(),
            OsString::from("--command"),
            OsString::from(terminal_restore_invocation(
                command.map(|(_, shell_command)| shell_command),
            )),
        ],
        description: format!("{kind} terminal"),
    })
}

fn terminal_restore_invocation(shell_command: Option<&str>) -> String {
    match shell_command {
        Some(shell_command) => {
            format!(r#"fish -lc 'exec bash -m -c \"{shell_command}; exec fish\"'"#)
        }
        None => "fish -l".into(),
    }
}

fn terminal_shell_command(
    kind: &str,
) -> Result<Option<(&'static str, &'static str)>, LaunchUnavailable> {
    let command = match kind {
        "shell" => None,
        "codex" => Some(("codex", "codex resume --last")),
        "agy" => Some(("agy", "agy -c")),
        "htop" => Some(("htop", "htop")),
        "nvtop" => Some(("nvtop", "nvtop")),
        _ => {
            return Err(LaunchUnavailable {
                code: LaunchUnavailableCode::UnsupportedTerminalKind,
                detail: format!("Unsupported terminal restore kind {kind}"),
            });
        }
    };
    Ok(command)
}

fn home_directory() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_directory();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_directory().join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopened_window_is_activated_and_raised() {
        assert_eq!(
            activation_args("{window-id}"),
            [
                "windowactivate",
                "{window-id}",
                "windowraise",
                "{window-id}"
            ]
        );
    }

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

    #[test]
    fn resolves_absolute_and_flatpak_desktop_entries() {
        let root = std::env::temp_dir().join(format!(
            "applicationlauncher-desktop-paths-{}",
            std::process::id()
        ));
        let direct = root.join("direct.desktop");
        let flatpak = root.join("flatpak/exports/share/applications");
        std::fs::create_dir_all(&flatpak).unwrap();
        std::fs::write(&direct, "[Desktop Entry]\nName=Direct\n").unwrap();
        std::fs::write(
            flatpak.join("org.example.Flatpak.desktop"),
            "[Desktop Entry]\nName=Flatpak\n",
        )
        .unwrap();

        let direct_restore = RestoreSpec {
            app_key: "direct".into(),
            desktop_file: Some(direct.display().to_string()),
            ..RestoreSpec::default()
        };
        assert_eq!(
            resolve_desktop_file_in(&direct_restore, &[]).unwrap(),
            direct
        );

        let flatpak_restore = RestoreSpec {
            app_key: "org.example.Flatpak".into(),
            desktop_file: Some("org.example.Flatpak".into()),
            ..RestoreSpec::default()
        };
        assert_eq!(
            resolve_desktop_file_in(&flatpak_restore, &[root.join("flatpak/exports/share")])
                .unwrap(),
            flatpak.join("org.example.Flatpak.desktop")
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_restore_kinds_are_explicit_and_unknown_kinds_are_rejected() {
        assert_eq!(terminal_shell_command("shell").unwrap(), None);
        for kind in ["codex", "agy", "htop", "nvtop"] {
            assert!(terminal_shell_command(kind).unwrap().is_some());
        }
        assert_eq!(
            terminal_shell_command("unknown").unwrap_err().code,
            LaunchUnavailableCode::UnsupportedTerminalKind
        );
    }

    #[test]
    fn terminal_restore_uses_a_monitor_mode_wrapper_for_job_control() {
        assert_eq!(
            terminal_restore_invocation(Some("codex resume --last")),
            r#"fish -lc 'exec bash -m -c \"codex resume --last; exec fish\"'"#
        );
        assert_eq!(terminal_restore_invocation(None), "fish -l");
    }

    #[test]
    fn absent_desktop_entries_have_a_stable_unavailable_reason() {
        let restore = RestoreSpec {
            app_key: "definitely-missing-applicationlauncher-test-entry".into(),
            ..RestoreSpec::default()
        };

        assert_eq!(
            resolve_desktop_file_in(&restore, &[]).unwrap_err().code,
            LaunchUnavailableCode::MissingDesktopEntry
        );
    }

    #[test]
    fn hidden_and_non_application_desktop_entries_are_not_reopenable() {
        let root = std::env::temp_dir().join(format!(
            "applicationlauncher-hidden-desktop-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let visible = root.join("visible.desktop");
        let hidden = root.join("hidden.desktop");
        let link = root.join("link.desktop");
        let missing_try_exec = root.join("missing-try-exec.desktop");
        std::fs::write(
            &visible,
            "[Desktop Entry]\nType=Application\nExec=example\n",
        )
        .unwrap();
        std::fs::write(
            &hidden,
            "[Desktop Entry]\nType=Application\nExec=example\nNoDisplay=true\n",
        )
        .unwrap();
        std::fs::write(
            &link,
            "[Desktop Entry]\nType=Link\nURL=https://example.com\n",
        )
        .unwrap();
        std::fs::write(
            &missing_try_exec,
            "[Desktop Entry]\nType=Application\nExec=example\nTryExec=/definitely/missing/applicationlauncher-test\n",
        )
        .unwrap();

        assert!(validate_desktop_file(&visible).is_ok());
        for path in [&hidden, &link] {
            assert_eq!(
                validate_desktop_file(path).unwrap_err().code,
                LaunchUnavailableCode::NonLaunchableDesktopEntry
            );
        }
        assert_eq!(
            validate_desktop_file(&missing_try_exec).unwrap_err().code,
            LaunchUnavailableCode::MissingExecutable
        );

        std::fs::remove_dir_all(root).unwrap();
    }
}
