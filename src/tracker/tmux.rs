use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::TmuxSession;

const QUERY_TIMEOUT: Duration = Duration::from_millis(250);
const CACHE_TTL: Duration = Duration::from_secs(2);
const MAX_SERVERS: usize = 16;
const MAX_CLIENTS: usize = 512;
const MAX_METADATA_BYTES: usize = 128 * 1024;
const CLIENT_FORMAT: &str =
    "#{client_pid}\t#{session_id}\t#{session_created}\t#{pid}\t#{session_name}";
const SESSION_FORMAT: &str = "#{session_id}\t#{session_created}\t#{pid}\t#{session_name}";

#[derive(Clone)]
struct ClientSession {
    pid: i32,
    session: TmuxSession,
}

struct CachedClients {
    checked_at: Instant,
    clients: Vec<ClientSession>,
}

pub(super) fn capture_session(
    client_pid: i32,
    arguments: &[String],
    cwd: Option<&str>,
) -> Option<TmuxSession> {
    if client_pid <= 0 {
        return None;
    }
    let process = PathBuf::from(format!("/proc/{client_pid}"));
    if std::fs::metadata(&process).ok()?.uid() != rustix::process::getuid().as_raw() {
        return None;
    }
    let mut environment = Vec::new();
    std::fs::File::open(process.join("environ"))
        .ok()?
        .take((MAX_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut environment)
        .ok()?;
    if environment.len() > MAX_METADATA_BYTES {
        return None;
    }
    let socket = socket_path(
        arguments,
        &environment,
        cwd,
        rustix::process::getuid().as_raw(),
    )?;
    let clients = cached_clients(&socket);
    let mut matches = clients.iter().filter(|client| client.pid == client_pid);
    let session = matches.next()?.session.clone();
    matches.next().is_none().then_some(session)
}

fn socket_path(
    arguments: &[String],
    environment: &[u8],
    cwd: Option<&str>,
    uid: u32,
) -> Option<PathBuf> {
    let env = |name: &str| {
        environment.split(|byte| *byte == 0).find_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            entry.strip_prefix(name)?.strip_prefix('=')
        })
    };
    let mut socket = None;
    let mut label = None;
    let mut args = arguments.iter().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--" => break,
            "-S" => socket = Some(args.next()?.as_str()),
            "-L" => label = Some(args.next()?.as_str()),
            "-f" | "-T" | "-c" => {
                args.next()?;
            }
            value if value.starts_with("-S") => socket = Some(&value[2..]),
            value if value.starts_with("-L") => label = Some(&value[2..]),
            value if value.starts_with('-') => {
                if !value[1..].chars().all(|flag| "2CDlNuUvV".contains(flag)) {
                    return None;
                }
            }
            _ => break,
        }
    }
    let path = if let Some(socket) = socket {
        PathBuf::from(socket)
    } else if label.is_none() && env("TMUX").is_some() {
        // TMUX is socket,server-pid,session-id; split from the right because a
        // socket directory may itself contain commas.
        PathBuf::from(env("TMUX")?.rsplitn(3, ',').nth(2)?)
    } else {
        let label = label.unwrap_or("default");
        if !valid_text(label) || label.contains('/') || label == "." || label == ".." {
            return None;
        }
        PathBuf::from(env("TMUX_TMPDIR").unwrap_or("/tmp"))
            .join(format!("tmux-{uid}"))
            .join(label)
    };
    let path = if path.is_absolute() {
        path
    } else {
        Path::new(cwd?).join(path)
    };
    (path.is_absolute() && valid_text(path.to_str()?)).then_some(path)
}

fn cached_clients(socket: &Path) -> Vec<ClientSession> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedClients>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let now = Instant::now();
    {
        let Ok(mut cache) = cache.try_lock() else {
            return Vec::new();
        };
        if let Some(entry) = cache
            .get(socket)
            .filter(|entry| now.duration_since(entry.checked_at) < CACHE_TTL)
        {
            return entry.clients.clone();
        }
        cache.retain(|_, entry| now.duration_since(entry.checked_at) < CACHE_TTL);
        if cache.len() >= MAX_SERVERS {
            return Vec::new();
        }
        // Reserve the refresh before I/O so concurrent callers never duplicate
        // queries or wait on a mutex held across a slow/unavailable server.
        cache.insert(
            socket.to_owned(),
            CachedClients {
                checked_at: now,
                clients: Vec::new(),
            },
        );
    }
    let clients = query(socket, "list-clients", CLIENT_FORMAT)
        .ok()
        .and_then(|text| parse_clients(socket, &text))
        .unwrap_or_default();
    if let Ok(mut cache) = cache.try_lock()
        && let Some(entry) = cache
            .get_mut(socket)
            .filter(|entry| entry.checked_at == now)
    {
        entry.clients = clients.clone();
    }
    clients
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

pub(super) fn valid_session(session: &TmuxSession) -> bool {
    Path::new(&session.socket_path).is_absolute()
        && valid_text(&session.socket_path)
        && valid_text(&session.session_name)
        && session.server_pid > 0
        && session.created_at > 0
        && session.session_id.strip_prefix('$').is_some_and(|id| {
            !id.is_empty()
                && id.len() <= 20
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && id.parse::<u64>().is_ok()
        })
}

fn parse_session(socket: &Path, fields: &[&str]) -> Option<TmuxSession> {
    if fields.len() != 4 {
        return None;
    }
    let session = TmuxSession {
        socket_path: socket.to_str()?.to_owned(),
        session_id: fields[0].into(),
        created_at: fields[1].parse().ok()?,
        server_pid: fields[2].parse().ok()?,
        session_name: fields[3].into(),
    };
    valid_session(&session).then_some(session)
}

fn parse_clients(socket: &Path, text: &str) -> Option<Vec<ClientSession>> {
    if text.len() > MAX_METADATA_BYTES {
        return None;
    }
    let mut clients = Vec::new();
    for line in text.lines() {
        if clients.len() >= MAX_CLIENTS {
            return None;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 5 {
            return None;
        }
        let pid = fields[0].parse().ok().filter(|pid| *pid > 0)?;
        clients.push(ClientSession {
            pid,
            session: parse_session(socket, &fields[1..])?,
        });
    }
    Some(clients)
}

fn query(socket: &Path, action: &str, format: &str) -> Result<String, String> {
    let metadata = std::fs::metadata(socket)
        .map_err(|_| "The saved tmux server socket is unavailable".to_owned())?;
    if !metadata.file_type().is_socket() || metadata.uid() != rustix::process::getuid().as_raw() {
        return Err("The tmux socket is not a socket owned by the current user".into());
    }
    let mut command =
        Command::new(crate::process::executable_path("tmux").ok_or("tmux is not installed")?);
    command
        .args(["-N", "-S"])
        .arg(socket)
        .args([action, "-F", format]);
    let output = crate::process::output_with_timeout(command, QUERY_TIMEOUT)
        .map_err(|err| format!("Could not query tmux: {err}"))?;
    if !output.status.success() || output.stdout.len() > MAX_METADATA_BYTES {
        return Err("The tmux server did not return usable session metadata".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "Invalid tmux session metadata encoding".into())
}

pub(super) fn same_session(left: &TmuxSession, right: &TmuxSession) -> bool {
    valid_session(left)
        && valid_session(right)
        && left.socket_path == right.socket_path
        && left.server_pid == right.server_pid
        && left.session_id == right.session_id
        && left.created_at == right.created_at
}

pub(super) fn verify_session(session: &TmuxSession) -> Result<(), String> {
    if !valid_session(session) {
        return Err("The saved tmux session identity is incomplete or invalid".into());
    }
    let text = query(
        Path::new(&session.socket_path),
        "list-sessions",
        SESSION_FORMAT,
    )?;
    if text
        .lines()
        .take(MAX_CLIENTS)
        .filter_map(|line| {
            parse_session(
                Path::new(&session.socket_path),
                &line.split('\t').collect::<Vec<_>>(),
            )
        })
        .any(|current| same_session(session, &current))
    {
        Ok(())
    } else {
        Err(format!(
            "Saved tmux session {:?} no longer exists; its processes cannot be reattached after the tmux server exits or the computer reboots",
            session.session_name
        ))
    }
}

pub(super) fn attach_arguments(session: &TmuxSession) -> Option<Vec<String>> {
    if !valid_session(session) {
        return None;
    }
    // Recheck inside the server's command queue, not only before spawning the
    // terminal: a restarted server can reuse $0 before the new client attaches.
    Some(vec![
        "-N".into(),
        "-S".into(),
        session.socket_path.clone(),
        "if-shell".into(),
        "-F".into(),
        "-t".into(),
        session.session_id.clone(),
        format!(
            "#{{&&:#{{==:#{{session_created}},{}}},#{{==:#{{pid}},{}}}}}",
            session.created_at, session.server_pid
        ),
        format!("attach-session -E -t '{}'", session.session_id),
        "display-message -p 'Saved tmux session is no longer available; no session was attached.'"
            .into(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(|arg| (*arg).into()).collect()
    }

    fn session() -> TmuxSession {
        TmuxSession {
            socket_path: "/tmp/tmux-test/default".into(),
            server_pid: 800,
            session_id: "$7".into(),
            session_name: "qwen".into(),
            created_at: 1788885095,
        }
    }

    #[test]
    fn client_mapping_keeps_different_sessions_and_two_clients_of_one_session() {
        let text = "100\t$7\t1788885095\t800\tqwen\n101\t$9\t1788974529\t800\tchatbot\n102\t$7\t1788885095\t800\tqwen\n";
        let clients = parse_clients(Path::new("/tmp/tmux-test/default"), text).unwrap();
        assert_eq!(clients.len(), 3);
        assert_eq!(clients[0].pid, 100);
        assert!(same_session(&clients[0].session, &clients[2].session));
        assert!(!same_session(&clients[0].session, &clients[1].session));
    }

    #[test]
    fn socket_selection_respects_custom_servers_and_client_environment() {
        let environment = b"TMUX=/tmp/outer,comma/socket,200,1\0TMUX_TMPDIR=/tmp/custom\0";
        for (argv, expected) in [
            (
                vec!["tmux", "new-session", "-s", "qwen"],
                "/tmp/outer,comma/socket",
            ),
            (
                vec!["tmux", "-L", "work", "attach"],
                "/tmp/custom/tmux-1000/work",
            ),
            (
                vec!["tmux", "-Lwork", "attach"],
                "/tmp/custom/tmux-1000/work",
            ),
            (
                vec!["tmux", "-S", "/tmp/explicit", "-L", "work", "attach"],
                "/tmp/explicit",
            ),
            (vec!["tmux", "-Srelative", "attach"], "/project/relative"),
            (
                vec![
                    "tmux",
                    "-f",
                    "/tmp/config",
                    "-C",
                    "-S/tmp/explicit",
                    "attach",
                ],
                "/tmp/explicit",
            ),
        ] {
            assert_eq!(
                socket_path(&args(&argv), environment, Some("/project"), 1000),
                Some(PathBuf::from(expected))
            );
        }
        assert_eq!(
            socket_path(&args(&["tmux", "attach"]), b"", None, 1000),
            Some("/tmp/tmux-1000/default".into())
        );
        for argv in [
            vec!["tmux", "-L", "../escape"],
            vec!["tmux", "-S"],
            vec!["tmux", "-Z", "attach"],
        ] {
            assert!(socket_path(&args(&argv), b"", None, 1000).is_none());
        }
    }

    #[test]
    fn malformed_or_oversized_client_metadata_is_not_partially_used() {
        for text in [
            "100\t$7\t123\t800\tgood\n101\t$9\t123\t800\tbad\tname\n".to_owned(),
            "0\t$7\t123\t800\tqwen\n".into(),
            "100\t$(touch /tmp/bad)\t123\t800\tqwen\n".into(),
            "100\t$7\t0\t800\tqwen\n".into(),
            "100\t$7\t123\t0\tqwen\n".into(),
            "100\t$7\t123\t800\tqwen\n".repeat(MAX_CLIENTS + 1),
            "x".repeat(MAX_METADATA_BYTES + 1),
        ] {
            assert!(parse_clients(Path::new("/tmp/tmux-test/default"), &text).is_none());
        }
    }

    #[test]
    fn session_identity_survives_rename_but_not_server_or_session_reuse() {
        let saved = session();
        let mut current = saved.clone();
        current.session_name = "renamed".into();
        assert!(same_session(&saved, &current));
        current.created_at += 1;
        assert!(!same_session(&saved, &current));
        current = saved.clone();
        current.socket_path = "/tmp/other-server/default".into();
        assert!(!same_session(&saved, &current));
        current = saved.clone();
        current.server_pid += 1;
        assert!(!same_session(&saved, &current));
        current = saved.clone();
        current.session_id = "$8".into();
        assert!(!same_session(&saved, &current));
    }

    #[test]
    fn attach_is_exact_non_detaching_and_does_not_replay_saved_names_as_commands() {
        let mut saved = session();
        saved.session_name = "a; run-shell 'touch /tmp/bad'".into();
        saved.socket_path = "/tmp/socket with 'quotes'; semicolons".into();
        let arguments = attach_arguments(&saved).unwrap();
        assert_eq!(arguments[0], "-N");
        assert_eq!(arguments[2], saved.socket_path);
        assert!(arguments.contains(&"attach-session -E -t '$7'".into()));
        assert!(
            arguments
                .contains(&"#{&&:#{==:#{session_created},1788885095},#{==:#{pid},800}}".into())
        );
        assert!(!arguments.iter().any(|arg| arg.contains("new-session")
            || arg.contains("kill-")
            || arg.contains(" -d")
            || arg.contains(" -x")
            || arg.contains(&saved.session_name)));
        saved.session_id = "$7; kill-server".into();
        assert!(attach_arguments(&saved).is_none());
    }

    #[test]
    fn missing_server_is_reported_without_creating_a_new_one() {
        let mut saved = session();
        saved.socket_path = format!(
            "/tmp/nonexistent-launcher-tmux-{}-socket",
            std::process::id()
        );
        assert!(verify_session(&saved).is_err());
        assert!(!Path::new(&saved.socket_path).exists());
    }

    struct PrivateServer {
        program: PathBuf,
        directory: PathBuf,
        socket: PathBuf,
    }

    impl PrivateServer {
        fn command(&self) -> Command {
            let mut command = Command::new(&self.program);
            command
                .args(["-N", "-S"])
                .arg(&self.socket)
                .env_remove("TMUX");
            command
        }

        fn clients(&self) -> Vec<ClientSession> {
            parse_clients(
                &self.socket,
                &query(&self.socket, "list-clients", CLIENT_FORMAT).unwrap(),
            )
            .unwrap()
        }

        fn wait_for_client(&self, pid: u32) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.clients().iter().any(|client| client.pid == pid as i32) {
                assert!(
                    Instant::now() < deadline,
                    "private control client failed to attach"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for PrivateServer {
        fn drop(&mut self) {
            // Only this test's freshly created private socket is ever addressed.
            let mut command = self.command();
            command.arg("kill-server");
            let _ = crate::process::output_with_timeout(command, Duration::from_secs(1));
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    struct TestClient(std::process::Child);

    impl Drop for TestClient {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn control_client(program: &Path, arguments: &[String]) -> TestClient {
        use std::process::Stdio;
        TestClient(
            Command::new(program)
                .arg("-C")
                .args(arguments)
                .env_remove("TMUX")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    #[test]
    #[ignore = "requires tmux; creates and removes only an isolated private test server"]
    fn private_server_capture_upgrade_reattach_and_stale_identity_guard() {
        use std::os::unix::fs::PermissionsExt;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("al-tmux-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = PrivateServer {
            program: crate::process::executable_path("tmux")
                .expect("tmux must be installed for this opt-in test"),
            socket: directory.join("socket"),
            directory,
        };
        let mut create = Command::new(&server.program);
        create.arg("-S").arg(&server.socket).args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "fixture",
            "sleep",
            "60",
        ]);
        assert!(
            crate::process::output_with_timeout(create, Duration::from_secs(2))
                .unwrap()
                .status
                .success()
        );
        let text = query(&server.socket, "list-sessions", SESSION_FORMAT).unwrap();
        let session = parse_session(
            &server.socket,
            &text.trim_end().split('\t').collect::<Vec<_>>(),
        )
        .unwrap();
        let original_args = vec![
            "-N".into(),
            "-S".into(),
            session.socket_path.clone(),
            "attach-session".into(),
            "-t".into(),
            session.session_id.clone(),
        ];
        let mut original = control_client(&server.program, &original_args);
        server.wait_for_client(original.0.id());
        let window = super::super::TrackedWindow {
            id: "private-tmux-window".into(),
            pid: original.0.id() as i32,
            class: "xfce4-terminal".into(),
            title: "codex inside tmux - Terminal".into(),
            ..Default::default()
        };
        let captured = super::super::infer_restore_spec(&window);
        assert_eq!(captured.terminal_kind.as_deref(), Some("tmux"));
        assert_eq!(captured.tmux_session.as_ref(), Some(&session));
        let upgraded = super::super::refresh_live_restore_spec(
            &window,
            super::super::RestoreSpec {
                terminal_kind: Some("shell".into()),
                ..Default::default()
            },
        );
        assert_eq!(upgraded.tmux_session.as_ref(), Some(&session));
        verify_session(&session).unwrap();

        let restored = control_client(&server.program, &attach_arguments(&session).unwrap());
        server.wait_for_client(restored.0.id());
        assert_eq!(server.clients().len(), 2);
        assert!(
            original.0.try_wait().unwrap().is_none(),
            "reattachment must not detach another client"
        );

        let mut stale = session.clone();
        stale.created_at += 1;
        assert!(verify_session(&stale).is_err());
        let mut rejected = Command::new(&server.program);
        rejected.arg("-C").args(attach_arguments(&stale).unwrap());
        let output = crate::process::output_with_timeout(rejected, Duration::from_secs(2)).unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("no longer available"));
        assert_eq!(
            server.clients().len(),
            2,
            "stale identity must not attach to a reused session ID"
        );
        drop(restored);
        drop(original);
        drop(server);
        assert!(verify_session(&session).is_err());
    }

    #[test]
    #[ignore = "reads live metadata only; requires APPLICATIONLAUNCHER_TEST_TMUX_TERMINAL_PIDS"]
    fn live_tmux_terminal_capture() {
        let pids = std::env::var("APPLICATIONLAUNCHER_TEST_TMUX_TERMINAL_PIDS")
            .expect("provide an explicit comma-separated list of live xfce4-terminal PIDs");
        let pids: Vec<i32> = pids.split(',').map(|pid| pid.parse().unwrap()).collect();
        assert!(!pids.is_empty() && pids.len() <= 16);
        for pid in pids {
            let executable = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
            assert!(
                executable
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("xfce4-terminal")
            );
            let window = super::super::TrackedWindow {
                pid,
                class: "xfce4-terminal".into(),
                title: "Terminal".into(),
                ..Default::default()
            };
            let restore = super::super::infer_restore_spec(&window);
            assert_eq!(
                restore.terminal_kind.as_deref(),
                Some("tmux"),
                "terminal {pid}"
            );
            let session = restore
                .tmux_session
                .expect("the exact live tmux client must be identified");
            assert!(valid_session(&session));
            verify_session(&session).unwrap();
            eprintln!(
                "terminal {pid}: captured tmux session {:?} ({})",
                session.session_name, session.session_id
            );
        }
    }
}
