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
    #[cfg(target_os = "linux")]
    {
        return collect_child_output(&mut child, timeout);
    }
    #[cfg(not(target_os = "linux"))]
    {
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
}

#[cfg(target_os = "linux")]
fn collect_child_output(child: &mut Child, timeout: Duration) -> io::Result<Output> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let mut group_cleaned = false;
    let result = (|| {
        let mut pipes = [
            child
                .stdout
                .take()
                .map(|pipe| std::fs::File::from(OwnedFd::from(pipe))),
            child
                .stderr
                .take()
                .map(|pipe| std::fs::File::from(OwnedFd::from(pipe))),
        ];
        for pipe in pipes.iter().flatten() {
            let fd = pipe.as_raw_fd();
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) };
        let pidfd = (pidfd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(pidfd as i32) });
        let deadline = Instant::now() + timeout;
        let mut output = [Vec::new(), Vec::new()];
        let mut status = None;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            if status.is_some() && pipes.iter().all(Option::is_none) {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut descriptors = [
                libc::pollfd {
                    fd: pipes[0].as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pipes[1].as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if status.is_none() {
                        pidfd.as_ref().map_or(-1, AsRawFd::as_raw_fd)
                    } else {
                        -1
                    },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let wait = if pidfd.is_none() {
                remaining.min(POLL_INTERVAL)
            } else {
                remaining
            };
            let milliseconds = wait
                .as_millis()
                .saturating_add(u128::from(wait.subsec_nanos() % 1_000_000 != 0))
                .min(i32::MAX as u128) as i32;
            let ready = unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    descriptors.len() as _,
                    milliseconds,
                )
            };
            if ready < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if status.is_none() {
                if pidfd.is_some() && descriptors[2].revents != 0 {
                    // The unreaped leader still owns its PID, so cleanup cannot hit a reused group.
                    terminate_process_group(child.id());
                    group_cleaned = true;
                    status = Some(child.wait()?);
                } else if pidfd.is_none() && child_exit_pending(child.id())? {
                    terminate_process_group(child.id());
                    group_cleaned = true;
                    status = Some(child.wait()?);
                }
            }
            for index in 0..2 {
                if descriptors[index].revents == 0 {
                    continue;
                }
                // Bound each drain so a continuous writer cannot starve the deadline or other pipe.
                for _ in 0..4 {
                    let Some(pipe) = pipes[index].as_mut() else {
                        break;
                    };
                    match pipe.read(&mut buffer) {
                        Ok(0) => {
                            pipes[index] = None;
                            break;
                        }
                        Ok(length) => {
                            let retained = length.min(
                                (MAX_CAPTURE_BYTES as usize).saturating_sub(output[index].len()),
                            );
                            output[index].extend_from_slice(&buffer[..retained]);
                        }
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(err) => return Err(err),
                    }
                }
            }
            if Instant::now() >= deadline && (status.is_none() || pipes.iter().any(Option::is_some))
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "command exceeded its deadline",
                ));
            }
        }
        let [stdout, stderr] = output;
        Ok(Output {
            status: status.unwrap(),
            stdout,
            stderr,
        })
    })();
    if result.is_err() && !group_cleaned {
        terminate_child(child);
    }
    result
}

#[cfg(target_os = "linux")]
fn child_exit_pending(pid: u32) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error);
    }
    // Observe without reaping: the PID cannot be reused before group cleanup.
    Ok(unsafe { info.si_pid() } == pid as i32)
}

pub fn status_with_timeout(command: Command, timeout: Duration) -> io::Result<ExitStatus> {
    output_with_timeout(command, timeout).map(|output| output.status)
}

pub fn spawn_and_reap(command: Command) -> io::Result<()> {
    spawn_and_reap_with_pid(command).map(|_| ())
}

/// Return the independently scoped child's identity without tying its lifetime to the daemon.
pub fn spawn_and_reap_with_pid(command: Command) -> io::Result<u32> {
    let mut child = spawn_in_independent_scope(command)?;
    let pid = child.id();
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
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
        // argv is already separated; systemd must not expand literal tmux $IDs.
        "--expand-environment=no",
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

#[cfg(not(target_os = "linux"))]
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

#[cfg(not(target_os = "linux"))]
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
    fn collector_drains_both_pipes_and_bounds_each_capture() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "head -c 5000000 /dev/zero & head -c 5000000 /dev/zero >&2 & wait",
        ]);
        let output = output_with_timeout(command, Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), MAX_CAPTURE_BYTES as usize);
        assert_eq!(output.stderr.len(), MAX_CAPTURE_BYTES as usize);
        assert!(output.stdout.iter().all(|byte| *byte == 0));
        assert!(output.stderr.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn collector_cleans_up_descendants_that_hold_pipes_after_leader_exit() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & printf 'ok'; exit 0"]);
        let output = output_with_timeout(command, Duration::from_secs(2)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"ok");
    }

    #[test]
    fn collector_handles_silent_children_closed_pipes_and_binary_output() {
        for script in ["exit 7", "exec 1>&- 2>&-; sleep 0.02; exit 7"] {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            let output = output_with_timeout(command, Duration::from_secs(2)).unwrap();
            assert_eq!(output.status.code(), Some(7));
            assert!(output.stdout.is_empty());
        }
        let mut command = Command::new("sh");
        command.args(["-c", "printf '\\377\\000\\376'; printf error >&2"]);
        let output = output_with_timeout(command, Duration::from_secs(2)).unwrap();
        assert_eq!(output.stdout, [255, 0, 254]);
        assert_eq!(output.stderr, b"error");
        assert_eq!(
            output_with_timeout(
                Command::new("/applicationlauncher-no-such-test-executable"),
                Duration::from_secs(1)
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pidfd_fallback_observes_exit_without_reaping_the_owned_child() {
        let mut child = Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child_exit_pending(child.id()).unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(child_exit_pending(child.id()).unwrap());
        assert_eq!(child.wait().unwrap().code(), Some(7));
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
    #[ignore = "requires the user systemd manager; launches only a private printf scope"]
    fn independent_scope_preserves_literal_tmux_ids_and_shell_arguments() {
        let mut command = Command::new(executable_path("printf").unwrap());
        let literals = [
            "$6",
            "${APPLICATIONLAUNCHER_SCOPE_TEST_VALUE}",
            "$APPLICATIONLAUNCHER_SCOPE_TEST_VALUE",
            "$argv",
            "attach-session -E -t '$6'",
            "/a directory/$6; not a command",
        ];
        command
            .arg("<%s>\n")
            .args(literals)
            .env("APPLICATIONLAUNCHER_SCOPE_TEST_VALUE", "must not expand");
        let (scoped, _) = independent_scope_command(command);
        let output = output_with_timeout(scoped, Duration::from_secs(3)).unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            literals.map(|value| format!("<{value}>\n")).concat()
        );
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
                .any(|argument| argument == "--expand-environment=no")
        );
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
