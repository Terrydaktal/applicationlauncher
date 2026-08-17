use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_applicationlauncher")
}

fn test_root(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    PathBuf::from(format!(
        "/tmp/applicationlauncher-{name}-{}-{nonce}",
        std::process::id()
    ))
}

fn socket_request(path: &Path, request: &str) -> String {
    let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn panic_probe_writes_a_private_symbolizable_report() {
    let root = test_root("panic");
    std::fs::create_dir_all(&root).unwrap();
    let output = Command::new(binary())
        .args(["--diagnostic-probe", "panic"])
        .env("XDG_STATE_HOME", &root)
        .env("APPLICATIONLAUNCHER_DIAGNOSTICS", "minimal")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report = root.join("applicationlauncher/panic-gui-latest.log");
    let contents = std::fs::read_to_string(&report).unwrap();
    assert!(contents.contains("intentional diagnostic panic probe"));
    assert!(contents.contains("build_id:"));
    let mode = std::fs::metadata(report).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn native_crash_probe_is_contained_and_observable() {
    let root = test_root("native-crash");
    std::fs::create_dir_all(&root).unwrap();
    let status = Command::new(binary())
        .args(["--diagnostic-probe", "native-crash"])
        .env("XDG_STATE_HOME", &root)
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGSEGV));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn diagnostic_endpoint_responds_while_the_main_thread_is_blocked() {
    let root = test_root("hang");
    let runtime = root.join("runtime");
    let state = root.join("state");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let mut child = Command::new(binary())
        .args(["--diagnostic-probe", "hang"])
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let socket = PathBuf::from(ready.trim().strip_prefix("ready ").unwrap());
    let ping = socket_request(&socket, "ping\n");
    assert!(ping.starts_with("ok gui ") || ping.starts_with("ok test "));
    let snapshot = socket_request(&socket, "snapshot\n");
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(snapshot["pid"].as_u64(), Some(child.id() as u64));

    let collector = Command::new(binary())
        .args(["--diagnose", "auto"])
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(
        collector.status.success(),
        "collector failed: {}",
        String::from_utf8_lossy(&collector.stderr)
    );
    let diagnostics_dir = state.join("applicationlauncher/diagnostics");
    let capture_dir = std::fs::read_dir(&diagnostics_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .max()
        .unwrap();
    let capture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(capture_dir.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["targets"].as_array().unwrap().len(), 1);
    assert!(capture_dir.join("manifest.sha256").is_file());
    let target_dir = capture_dir.join(format!("gui-{}", child.id()));
    assert!(target_dir.join("semantic-snapshot.json").is_file());
    assert!(target_dir.join("proc/status.txt").is_file());

    child.kill().unwrap();
    child.wait().unwrap();
    let _ = std::fs::remove_file(&socket);
    if let Some(parent) = socket.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn test_binary_contains_an_elf_build_id_and_symbolization_data() {
    let notes = Command::new("readelf")
        .args(["-n", binary()])
        .output()
        .unwrap();
    assert!(notes.status.success());
    assert!(String::from_utf8_lossy(&notes.stdout).contains("Build ID:"));

    let sections = Command::new("readelf")
        .args(["-S", binary()])
        .output()
        .unwrap();
    assert!(sections.status.success());
    assert!(String::from_utf8_lossy(&sections.stdout).contains(".debug_info"));
}
