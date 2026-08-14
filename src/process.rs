use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_CAPTURE_BYTES: u64 = 4 * 1024 * 1024;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SPAWN_SCOPE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const SPAWN_SCOPE_TIMEOUT: Duration = Duration::from_secs(2);

pub fn kdotool_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    kdotool_path_for_home(home.as_deref())
}

pub fn executable_path(program: impl AsRef<Path>) -> Option<PathBuf> {
    let program = program.as_ref();
    if program.components().count() > 1 {
        return is_executable_file(program).then(|| program.to_path_buf());
    }

    executable_path_in(program, &executable_search_directories())
}

fn executable_search_directories() -> Vec<PathBuf> {
    let mut directories = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        directories.push(home.join(".local/bin"));
        directories.push(home.join(".cargo/bin"));
    }
    directories.extend([PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")]);
    let mut unique = Vec::with_capacity(directories.len());
    for directory in directories {
        if !unique.contains(&directory) {
            unique.push(directory);
        }
    }
    unique
}

fn executable_path_in(program: &Path, directories: &[PathBuf]) -> Option<PathBuf> {
    directories
        .iter()
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn kdotool_path_for_home(home: Option<&std::path::Path>) -> std::path::PathBuf {
    if let Some(home) = home {
        for relative in [".local/bin/kdotool", ".cargo/bin/kdotool"] {
            let candidate = home.join(relative);
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    std::path::PathBuf::from("kdotool")
}

pub fn output_with_timeout(mut command: Command, timeout: Duration) -> io::Result<Output> {
    isolate_process_group(&mut command);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().map(read_pipe);
    let stderr = child.stderr.take().map(read_pipe);
    let deadline = Instant::now() + timeout;

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            terminate_child(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "command exceeded its deadline",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    };

    // A command can exit while a descendant still owns one of its pipes.
    // Reap the leader and terminate the private group before joining readers.
    terminate_process_group(child.id());

    Ok(Output {
        status,
        stdout: join_pipe(stdout),
        stderr: join_pipe(stderr),
    })
}

pub fn status_with_timeout(command: Command, timeout: Duration) -> io::Result<ExitStatus> {
    output_with_timeout(command, timeout).map(|output| output.status)
}

pub fn spawn_and_reap(command: Command) -> io::Result<()> {
    let mut child = spawn_in_independent_scope(command)?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn spawn_in_independent_scope(command: Command) -> io::Result<Child> {
    let (mut scoped, expected_cgroup) = independent_scope_command(command);
    let mut child = scoped.spawn()?;
    let deadline = Instant::now() + SPAWN_SCOPE_TIMEOUT;
    loop {
        let cgroup =
            std::fs::read_to_string(format!("/proc/{}/cgroup", child.id())).unwrap_or_default();
        if cgroup.lines().any(|line| line.contains(&expected_cgroup)) {
            break;
        }
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Err(io::Error::other(
                    "the isolated application exited before its scope could be verified",
                ));
            } else {
                return Err(io::Error::other(format!(
                    "systemd-run could not start the isolated application scope ({status})"
                )));
            }
        }
        if Instant::now() >= deadline {
            thread::spawn(move || {
                let _ = child.wait();
            });
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "could not verify the application's independent systemd scope",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }

    Ok(child)
}

fn independent_scope_command(command: Command) -> (Command, String) {
    let program = command.get_program().to_os_string();
    let arguments = command
        .get_args()
        .map(std::ffi::OsStr::to_os_string)
        .collect::<Vec<_>>();
    let current_dir = command.get_current_dir().map(std::path::Path::to_path_buf);
    let environment = command
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
        .collect::<Vec<_>>();
    let sequence = SPAWN_SCOPE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let launch_nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let scope_name = format!(
        "applicationlauncher-spawn-{}-{launch_nonce}-{sequence}",
        std::process::id(),
    );
    let expected_cgroup = format!("{scope_name}.scope");

    let mut scoped = Command::new("systemd-run");
    scoped.args([
        "--user",
        "--scope",
        "--collect",
        "--quiet",
        "--unit",
        &scope_name,
        "--",
    ]);
    scoped.arg(program).args(arguments);
    if let Some(current_dir) = current_dir {
        scoped.current_dir(current_dir);
    }
    for (key, value) in environment {
        if let Some(value) = value {
            scoped.env(key, value);
        } else {
            scoped.env_remove(key);
        }
    }

    (scoped, expected_cgroup)
}

pub fn atomic_write(path: &std::path::Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(
                std::fs::metadata(path)
                    .map(|metadata| metadata.permissions().mode() & 0o777)
                    .unwrap_or(0o600),
            ))?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn read_pipe(pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut limited = pipe.take(MAX_CAPTURE_BYTES.saturating_add(1));
        let _ = limited.read_to_end(&mut output);
        if output.len() > MAX_CAPTURE_BYTES as usize {
            output.truncate(MAX_CAPTURE_BYTES as usize);
        }
        output
    })
}

fn join_pipe(pipe: Option<thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    pipe.and_then(|thread| thread.join().ok())
        .unwrap_or_default()
}

fn terminate_child(child: &mut Child) {
    terminate_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

fn terminate_process_group(child_id: u32) {
    #[cfg(unix)]
    {
        if let Some(pid) = rustix::process::Pid::from_raw(child_id as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

#[cfg(unix)]
fn isolate_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // Keep timeout cleanup from leaving descendants holding stdout/stderr open.
    unsafe {
        command.pre_exec(|| rustix::process::setpgid(None, None).map_err(io::Error::from));
    }
}

#[cfg(not(unix))]
fn isolate_process_group(_command: &mut Command) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn kdotool_resolves_cargo_install_outside_path() {
        let root = std::env::temp_dir().join(format!(
            "applicationlauncher-kdotool-path-{}",
            std::process::id()
        ));
        let cargo_bin = root.join(".cargo/bin");
        std::fs::create_dir_all(&cargo_bin).unwrap();
        let installed = cargo_bin.join("kdotool");
        std::fs::write(&installed, b"test").unwrap();

        assert_eq!(kdotool_path_for_home(Some(&root)), installed);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn executable_resolution_preserves_search_order_and_requires_execute_permission() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "applicationlauncher-executable-path-{}",
            std::process::id()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let first_program = first.join("example-program");
        let second_program = second.join("example-program");
        std::fs::write(&first_program, b"first").unwrap();
        std::fs::write(&second_program, b"second").unwrap();
        std::fs::set_permissions(&first_program, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&second_program, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(
            executable_path_in(Path::new("example-program"), &[first, second.clone()]),
            Some(second_program)
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timeout_terminates_a_command_with_descendants() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & wait"]);
        let started = Instant::now();
        let result = output_with_timeout(command, Duration::from_millis(100));

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    #[ignore = "requires the live user systemd manager"]
    fn launched_applications_enter_an_independent_systemd_scope() {
        let mut command = Command::new("sleep");
        command.arg("1");
        let mut child = spawn_in_independent_scope(command).unwrap();
        let cgroup = std::fs::read_to_string(format!("/proc/{}/cgroup", child.id())).unwrap();

        assert!(cgroup.contains("applicationlauncher-spawn-"));
        assert!(cgroup.contains(".scope"));
        let _ = child.wait();
    }

    #[test]
    fn application_launches_are_wrapped_in_collectable_user_scopes() {
        let mut command = Command::new("example-program");
        command.arg("--example");
        let (scoped, expected_cgroup) = independent_scope_command(command);
        let arguments = scoped
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(scoped.get_program(), "systemd-run");
        assert!(arguments.starts_with(&[
            "--user".into(),
            "--scope".into(),
            "--collect".into(),
            "--quiet".into(),
            "--unit".into(),
        ]));
        assert!(arguments.iter().any(|argument| argument == "--"));
        assert!(
            arguments
                .iter()
                .any(|argument| argument == "example-program")
        );
        assert!(arguments.iter().any(|argument| argument == "--example"));
        assert!(expected_cgroup.starts_with("applicationlauncher-spawn-"));
        assert!(expected_cgroup.ends_with(".scope"));
    }
}
