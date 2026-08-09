use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

    Ok(Output {
        status,
        stdout: join_pipe(stdout),
        stderr: join_pipe(stderr),
    })
}

pub fn status_with_timeout(command: Command, timeout: Duration) -> io::Result<ExitStatus> {
    output_with_timeout(command, timeout).map(|output| output.status)
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

fn read_pipe(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        let _ = pipe.read_to_end(&mut output);
        output
    })
}

fn join_pipe(pipe: Option<thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    pipe.and_then(|thread| thread.join().ok())
        .unwrap_or_default()
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
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
    fn timeout_terminates_a_command_with_descendants() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & wait"]);
        let started = Instant::now();
        let result = output_with_timeout(command, Duration::from_millis(100));

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
