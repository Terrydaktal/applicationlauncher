use crate::models::WindowInfo;
use crate::*;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn launch_app(exec: &str) {
    let Some(mut argv) = desktop_exec_argv(exec) else {
        applicationlauncher::diagnostic_log!("Could not parse desktop Exec value");
        return;
    };
    let executable = argv.remove(0);
    std::thread::spawn(move || {
        let mut cmd = Command::new(executable);
        cmd.args(argv);

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

        let _ = applicationlauncher::process::spawn_and_reap(cmd);
    });
}

pub(crate) fn show_app_in_progs(app: &AppInfo) {
    let desktop_file = app.desktop_file_path.clone();
    std::thread::spawn(move || {
        let command = progs_command(&desktop_file);
        if let Err(error) = applicationlauncher::process::spawn_and_reap(command) {
            applicationlauncher::diagnostic_log!("Could not open application in Progs: {error}");
        }
    });
}

fn progs_command(desktop_file: &Path) -> Command {
    let executable = applicationlauncher::process::executable_path("progs")
        .unwrap_or_else(|| PathBuf::from("progs"));
    let mut command = Command::new(executable);
    command.arg("--show-desktop-file").arg(desktop_file);
    scrub_command_env(&mut command);
    command
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
        let cmd = run_in_terminal_command(&command);
        let _ = applicationlauncher::process::spawn_and_reap(cmd);
    });
}

fn run_in_terminal_command(command: &str) -> Command {
    // Repaint queues execution after the pending buffer is applied, even in Fish
    // builds that otherwise execute reader functions immediately.
    const INIT: &str = r#"
function __applicationlauncher_run_command --on-event fish_prompt
    builtin functions --erase __applicationlauncher_run_command
    builtin commandline --replace -- "$APPLICATIONLAUNCHER_TERMINAL_COMMAND"
    or return
    builtin set --erase --global APPLICATIONLAUNCHER_TERMINAL_COMMAND
    builtin commandline --function repaint execute
end
"#;
    let mut cmd = Command::new("xfce4-terminal");
    cmd.args(["--execute", "fish", "--interactive", "--init-command", INIT])
        .env("APPLICATIONLAUNCHER_TERMINAL_COMMAND", command);
    scrub_command_env(&mut cmd);
    cmd
}

pub(crate) fn launch_fish_terminal(
    cd_target: Option<String>,
    command_after_cd: Option<String>,
    terminal_title: Option<String>,
) {
    std::thread::spawn(move || {
        let cmd = fish_terminal_command(
            cd_target.as_deref(),
            command_after_cd.as_deref(),
            terminal_title.as_deref(),
        );
        let _ = applicationlauncher::process::spawn_and_reap(cmd);
    });
}

fn fish_terminal_command(
    cd_target: Option<&str>,
    command_after_cd: Option<&str>,
    terminal_title: Option<&str>,
) -> Command {
    let title_command = if terminal_title.is_some() {
        r#"printf '\e]0;%s\a' "$APPLICATIONLAUNCHER_TERMINAL_TITLE"; "#
    } else {
        ""
    };
    let mut fish_command = format!(
        r#"{title_command}if test -n "$APPLICATIONLAUNCHER_CD_TARGET"; cd "$APPLICATIONLAUNCHER_CD_TARGET"; end; "#
    );
    if let Some(command) = command_after_cd {
        fish_command.push_str(command);
        fish_command.push_str("; ");
    }
    fish_command.push_str("exec fish");

    let mut cmd = Command::new("xfce4-terminal");
    if let Some(title) = terminal_title {
        cmd.arg("--title").arg(title);
    }
    // --execute preserves argv; --command would parse and strip the Fish quotes again.
    cmd.args(["--execute", "fish", "-ic", &fish_command])
        .env(
            "APPLICATIONLAUNCHER_TERMINAL_TITLE",
            terminal_title.unwrap_or(""),
        )
        .env("APPLICATIONLAUNCHER_CD_TARGET", cd_target.unwrap_or(""));
    scrub_command_env(&mut cmd);
    cmd
}

pub(crate) fn launch_terminal_window() {
    std::thread::spawn(move || {
        let mut cmd = Command::new("xfce4-terminal");
        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("PYTHONHOME");
        cmd.env_remove("VIRTUAL_ENV");
        cmd.env_remove("UV_ACTIVE");
        let _ = applicationlauncher::process::spawn_and_reap(cmd);
    });
}

pub(crate) fn scrub_command_env(command: &mut Command) {
    command.env_remove("PYTHONPATH");
    command.env_remove("PYTHONHOME");
    command.env_remove("VIRTUAL_ENV");
    command.env_remove("UV_ACTIVE");
}

pub(crate) fn clone_terminal_command_for_window(win: &WindowInfo) -> Option<String> {
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
        let saved_arguments = codex_clone_arguments(&win.process_chain, win.pid, read_proc_cmdline);
        Some(applicationlauncher::tracker::codex_resume_arguments(&saved_arguments).join(" "))
    } else if matches("agy") {
        Some("agy -c".into())
    } else if matches("htop") {
        Some("htop".into())
    } else {
        None
    }
}

fn codex_clone_arguments(
    chain: &[crate::models::ProcessChainEntry],
    terminal_pid: Option<i32>,
    mut read_arguments: impl FnMut(i32) -> Option<Vec<String>>,
) -> Vec<String> {
    let Some(terminal_index) =
        terminal_pid.and_then(|pid| chain.iter().position(|entry| entry.pid == pid))
    else {
        return Vec::new();
    };
    // Chains run leaf-to-root. Prefer the owning Codex over nested agents, and
    // never borrow options from an ancestor above this terminal window.
    chain[..terminal_index]
        .iter()
        .rev()
        .find_map(|entry| {
            let arguments = read_arguments(entry.pid)?;
            let program = arguments.first()?;
            let is_codex = Path::new(program)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "codex" || name.starts_with("codex-code-mode"));
            is_codex.then(|| applicationlauncher::tracker::codex_restore_arguments(&arguments))
        })
        .unwrap_or_default()
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
    applicationlauncher::process::spawn_and_reap(command).is_ok()
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
    let output = applicationlauncher::process::output_with_timeout(
        command,
        std::time::Duration::from_secs(3),
    )
    .ok()?;
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
        let _ = applicationlauncher::process::spawn_and_reap(cmd);
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
    applicationlauncher::process::spawn_and_reap(command).is_ok()
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
    applicationlauncher::process::spawn_and_reap(command).is_ok()
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
    let mut cmd = Command::new("gtk-launch");
    cmd.arg(desktop_id);
    cmd.env_remove("PYTHONPATH");
    cmd.env_remove("VIRTUAL_ENV");
    cmd.env_remove("UV_ACTIVE");
    applicationlauncher::process::spawn_and_reap(cmd).is_ok()
}

#[cfg(test)]
mod progs_tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn progs_opens_the_exact_desktop_file_without_shell_interpretation() {
        let desktop_file = Path::new("/tmp/John's apps/$(not-a-command); browser.desktop");
        let command = progs_command(desktop_file);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("--show-desktop-file"), desktop_file.as_os_str()]
        );
        assert!(Path::new(command.get_program()).ends_with("progs"));
        for name in ["PYTHONPATH", "PYTHONHOME", "VIRTUAL_ENV", "UV_ACTIVE"] {
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == name && value.is_none())
            );
        }
    }
}

#[cfg(test)]
mod codex_clone_tests {
    use super::*;

    fn chain(pids: &[i32]) -> Vec<crate::models::ProcessChainEntry> {
        pids.iter()
            .map(|pid| crate::models::ProcessChainEntry {
                pid: *pid,
                name: String::new(),
                exe_path: None,
            })
            .collect()
    }

    #[test]
    fn clone_preserves_the_owning_codex_options_not_nested_agent_options() {
        let chain = chain(&[30, 20, 10, 1]);
        let mut reads = Vec::new();
        let arguments = codex_clone_arguments(&chain, Some(10), |pid| {
            reads.push(pid);
            Some(if pid == 20 {
                vec!["codex".into(), "--yolo".into()]
            } else {
                vec!["codex".into()]
            })
        });
        assert_eq!(arguments, ["--dangerously-bypass-approvals-and-sandbox"]);
        assert_eq!(reads, vec![20]);
        let arguments = codex_clone_arguments(&chain, Some(10), |pid| {
            Some(if pid == 20 {
                vec!["codex".into()]
            } else {
                vec!["codex".into(), "--yolo".into()]
            })
        });
        assert!(arguments.is_empty());
    }

    #[test]
    fn clone_does_not_read_an_unrelated_ancestor_or_an_unidentified_terminal() {
        let chain = chain(&[20, 10, 1]);
        let arguments = codex_clone_arguments(&chain, Some(10), |pid| {
            assert_eq!(pid, 20);
            Some(vec!["fish".into()])
        });
        assert!(arguments.is_empty());
        for pid in [None, Some(99)] {
            assert!(
                codex_clone_arguments(&chain, pid, |_| panic!("unidentified terminal")).is_empty()
            );
        }
    }
}

#[cfg(test)]
mod fish_launch_tests {
    use super::*;
    use std::ffi::OsStr;

    fn environment<'a>(command: &'a Command, name: &str) -> Option<Option<&'a OsStr>> {
        command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new(name))
            .map(|(_, value)| value)
    }

    #[test]
    fn run_in_terminal_uses_one_interactive_reader_and_literal_command_environment() {
        let input = "printf '%s\\n' \"quoted 'value'\"; printf '%s' '(literal) $HOME'";
        let command = run_in_terminal_command(input);
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(command.get_program(), "xfce4-terminal");
        assert_eq!(
            &arguments[..4],
            ["--execute", "fish", "--interactive", "--init-command"]
        );
        assert_eq!(arguments.len(), 5);
        let init = arguments[4].to_str().unwrap();
        assert!(init.contains("--on-event fish_prompt"));
        assert!(init.contains("commandline --replace --"));
        assert!(init.contains("commandline --function repaint execute"));
        assert!(!init.contains("eval "));
        assert!(!init.contains("exec fish"));
        assert!(!init.contains(input));
        assert_eq!(
            environment(&command, "APPLICATIONLAUNCHER_TERMINAL_COMMAND"),
            Some(Some(OsStr::new(input)))
        );
        for name in ["PYTHONPATH", "PYTHONHOME", "VIRTUAL_ENV", "UV_ACTIVE"] {
            assert_eq!(environment(&command, name), Some(None));
        }
    }

    fn check_interactive_command(input: &str, status: i32, expected: &str, state: &str) {
        let command = run_in_terminal_command(input);
        let mut fixture = Command::new("/usr/bin/python3");
        fixture.args(["-I", "-B"]);
        fixture.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fish_command_reader.py"));
        fixture.args([
            "--expect-status",
            &status.to_string(),
            "--expect-output",
            expected,
            "--expect-state",
            state,
            "--",
        ]);
        fixture.args(command.get_args().skip(1));
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => {
                    fixture.env(key, value);
                }
                None => {
                    fixture.env_remove(key);
                }
            }
        }
        let output = applicationlauncher::process::output_with_timeout(
            fixture,
            std::time::Duration::from_secs(15),
        )
        .expect("Fish reader PTY fixture must finish within its deadline");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn run_in_terminal_unknown_command_has_normal_error_timing_history_and_status() {
        check_interactive_command("applicationlauncher_no_such_command_19b3", 127, "", "");
    }

    #[test]
    fn run_in_terminal_keeps_quotes_multiline_commands_and_shell_state_in_one_reader() {
        check_interactive_command(
            "set -g applicationlauncher_reader_state 'state retained'\nprintf '%s\\n' \"quoted 'value'\" 'literal $HOME; (not-code)'",
            0,
            "quoted 'value'\nliteral $HOME; (not-code)\n",
            "state retained",
        );
    }

    #[test]
    fn run_in_terminal_preserves_failed_command_status_without_restarting_fish() {
        check_interactive_command("false", 1, "", "");
    }

    #[test]
    fn run_in_terminal_clears_the_handoff_before_the_command_and_its_children_run() {
        check_interactive_command(
            r#"if set --query APPLICATIONLAUNCHER_TERMINAL_COMMAND; echo 'handoff leaked'; else; command /bin/sh -c 'test -z "${APPLICATIONLAUNCHER_TERMINAL_COMMAND+x}" && printf "%s\n" "handoff cleared before execution"'; end"#,
            0,
            "handoff cleared before execution\n",
            "",
        );
    }

    #[test]
    fn fish_clone_preserves_script_quotes_in_separate_terminal_arguments() {
        let directory = "/tmp/girl's \"notes\"; (printf not-code)";
        for title in [None, Some("tmux: girls - girl's \"terminal\"; (not-code)")] {
            for after in [None, Some("printf '%s\\n' 'quoted value'")] {
                let command = fish_terminal_command(Some(directory), after, title);
                let arguments: Vec<_> = command.get_args().collect();
                assert_eq!(command.get_program(), "xfce4-terminal");
                assert!(!arguments.contains(&OsStr::new("--command")));
                let execute = arguments
                    .iter()
                    .position(|arg| *arg == "--execute")
                    .unwrap();
                assert_eq!(
                    &arguments[execute..execute + 3],
                    ["--execute", "fish", "-ic"]
                );
                assert_eq!(arguments.len(), execute + 4);
                let script = arguments[execute + 3].to_str().unwrap();
                assert!(script.ends_with("; exec fish"));
                if let Some(title) = title {
                    assert_eq!(&arguments[..execute], ["--title", title]);
                    assert!(script.starts_with(
                        r#"printf '\e]0;%s\a' "$APPLICATIONLAUNCHER_TERMINAL_TITLE"; "#
                    ));
                    assert!(!script.contains(title));
                } else {
                    assert_eq!(execute, 0);
                    assert!(!script.starts_with("printf "));
                }
                if let Some(after) = after {
                    assert!(script.ends_with(&format!("{after}; exec fish")));
                }
                assert!(!script.contains(directory));
                assert_eq!(
                    environment(&command, "APPLICATIONLAUNCHER_CD_TARGET"),
                    Some(Some(OsStr::new(directory)))
                );
                assert_eq!(
                    environment(&command, "APPLICATIONLAUNCHER_TERMINAL_TITLE"),
                    Some(Some(OsStr::new(title.unwrap_or(""))))
                );
            }
        }
    }

    #[test]
    fn fish_terminal_defaults_keep_empty_data_and_scrub_the_parent_runtime_environment() {
        let command = fish_terminal_command(None, None, None);
        for name in [
            "APPLICATIONLAUNCHER_CD_TARGET",
            "APPLICATIONLAUNCHER_TERMINAL_TITLE",
        ] {
            assert_eq!(environment(&command, name), Some(Some(OsStr::new(""))));
        }
        for name in ["PYTHONPATH", "PYTHONHOME", "VIRTUAL_ENV", "UV_ACTIVE"] {
            assert_eq!(environment(&command, name), Some(None));
        }
    }

    #[test]
    fn fish_clone_title_and_directory_work_in_real_fish_without_command_reparsing() {
        struct Directory(PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir(&self.0);
            }
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = Directory(std::env::temp_dir().join(format!(
            "al-fish-clone-{}-{nonce}-girl's \"notes\"; (not-code)",
            std::process::id()
        )));
        std::fs::create_dir(&directory.0).unwrap();
        let path = directory.0.to_str().unwrap();
        let title = "tmux: girls - \u{03c0} - girl's \"terminal\"; (not-code)";
        let command = fish_terminal_command(
            Some(path),
            Some(r#"printf '%s\n' "$PWD" "quoted 'value'""#),
            Some(title),
        );
        let script = command.get_args().last().unwrap().to_str().unwrap();
        // Exercise the real prefix and command, without starting an interactive shell.
        let script = script.strip_suffix("; exec fish").unwrap();
        let mut fish = Command::new("fish");
        fish.args(["--no-config", "-c", script]);
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => {
                    fish.env(key, value);
                }
                None => {
                    fish.env_remove(key);
                }
            }
        }
        let output = applicationlauncher::process::output_with_timeout(
            fish,
            std::time::Duration::from_secs(5),
        )
        .expect("Fish is required to verify terminal launch scripts");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("\x1b]0;{title}\x07{path}\nquoted 'value'\n")
        );
    }
}
