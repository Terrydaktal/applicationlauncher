use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

#[derive(Default)]
struct State {
    pending: HashMap<PathBuf, (u64, Vec<u8>)>,
    sequence: u64,
    writing: bool,
    stopping: bool,
    stopped: bool,
    errors: HashMap<PathBuf, String>,
}

struct Writer {
    shared: Arc<(Mutex<State>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Lifecycle(Arc<(Mutex<State>, Condvar)>);
impl Drop for Lifecycle {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap_or_else(|err| err.into_inner());
        state.stopped = true;
        self.0.1.notify_all();
    }
}

impl Writer {
    fn new(mut write: impl FnMut(&Path, &[u8]) -> io::Result<()> + Send + 'static) -> Self {
        let shared: Arc<(Mutex<State>, Condvar)> = Arc::default();
        let worker_state = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("settings-writer".into())
            .spawn(move || {
                let _lifecycle = Lifecycle(Arc::clone(&worker_state));
                let worker = applicationlauncher::observability::register_worker("settings-writer");
                loop {
                    worker.set_state("waiting");
                    let (path, bytes) = {
                        let mut state =
                            worker_state.0.lock().unwrap_or_else(|err| err.into_inner());
                        while state.pending.is_empty() && !state.stopping {
                            state = worker_state
                                .1
                                .wait(state)
                                .unwrap_or_else(|err| err.into_inner());
                        }
                        if state.pending.is_empty() {
                            break;
                        }
                        let path = state
                            .pending
                            .iter()
                            .min_by_key(|(_, (sequence, _))| *sequence)
                            .unwrap()
                            .0
                            .clone();
                        let (_, bytes) = state.pending.remove(&path).unwrap();
                        state.writing = true;
                        (path, bytes)
                    };
                    worker.set_state("atomic-write");
                    let result = write(&path, &bytes);
                    let mut state = worker_state.0.lock().unwrap_or_else(|err| err.into_inner());
                    match result {
                        Ok(()) => {
                            state.errors.remove(&path);
                        }
                        Err(err) => {
                            eprintln!("Could not save {}: {err}", path.display());
                            state.errors.insert(path, err.to_string());
                        }
                    }
                    state.writing = false;
                    worker_state.1.notify_all();
                }
            })
            .expect("could not start settings writer");
        Self {
            shared,
            thread: Some(thread),
        }
    }

    fn submit(&self, path: PathBuf, bytes: Vec<u8>) -> io::Result<()> {
        let mut state = self.shared.0.lock().unwrap_or_else(|err| err.into_inner());
        if state.stopping || state.stopped {
            return Err(io::Error::other("settings writer stopped"));
        }
        if state.pending.len() >= 3 && !state.pending.contains_key(&path) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "settings queue is full",
            ));
        }
        state.sequence += 1;
        let sequence = state.sequence;
        state.pending.insert(path, (sequence, bytes));
        self.shared.1.notify_one();
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        let mut state = self.shared.0.lock().unwrap_or_else(|err| err.into_inner());
        while (!state.pending.is_empty() || state.writing) && !state.stopped {
            state = self
                .shared
                .1
                .wait(state)
                .unwrap_or_else(|err| err.into_inner());
        }
        if state.stopped && (!state.pending.is_empty() || state.writing) {
            return Err(io::Error::other("settings worker failed"));
        }
        if let Some(error) = state.errors.values().next() {
            return Err(io::Error::other(error.clone()));
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .stopping = true;
        self.shared.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

static WRITER: OnceLock<Writer> = OnceLock::new();

pub(crate) fn queue_write(path: PathBuf, content: String) {
    let writer = WRITER.get_or_init(|| {
        Writer::new(|path, bytes| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            applicationlauncher::process::atomic_write(path, bytes)
        })
    });
    if let Err(err) = writer.submit(path, content.into_bytes()) {
        eprintln!("Could not queue settings: {err}");
    }
}

pub(crate) fn flush_writes() {
    if let Some(writer) = WRITER.get()
        && let Err(err) = writer.flush()
    {
        eprintln!("Could not flush settings: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_writer_coalesces_bounded_pending_writes_and_preserves_order() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&writes);
        let mut first = true;
        let writer = Writer::new(move |path, bytes| {
            if first {
                first = false;
                started_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
            }
            captured
                .lock()
                .unwrap()
                .push((path.to_path_buf(), bytes.to_vec()));
            Ok(())
        });
        writer
            .submit("settings".into(), b"initial".to_vec())
            .unwrap();
        started_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        for version in 0..1000 {
            writer
                .submit("settings".into(), version.to_string().into_bytes())
                .unwrap();
        }
        writer.submit("pins".into(), b"pins".to_vec()).unwrap();
        writer.submit("size".into(), b"size".to_vec()).unwrap();
        assert_eq!(writer.shared.0.lock().unwrap().pending.len(), 3);
        assert_eq!(
            writer.submit("fourth".into(), vec![]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        release_tx.send(()).unwrap();
        writer.flush().unwrap();
        assert_eq!(
            *writes.lock().unwrap(),
            vec![
                (PathBuf::from("settings"), b"initial".to_vec()),
                (PathBuf::from("settings"), b"999".to_vec()),
                (PathBuf::from("pins"), b"pins".to_vec()),
                (PathBuf::from("size"), b"size".to_vec())
            ]
        );
    }

    #[test]
    fn settings_writer_drop_flushes_the_latest_value() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        {
            let captured = Arc::clone(&writes);
            let writer = Writer::new(move |_, bytes| {
                captured.lock().unwrap().push(bytes.to_vec());
                Ok(())
            });
            writer.submit("settings".into(), b"one".to_vec()).unwrap();
            writer.submit("settings".into(), b"two".to_vec()).unwrap();
        }
        assert_eq!(writes.lock().unwrap().last().unwrap(), b"two");
    }

    #[test]
    fn settings_writer_reports_write_and_worker_failures_without_hanging() {
        let writer = Writer::new(|_, _| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "test failure",
            ))
        });
        writer.submit("settings".into(), vec![]).unwrap();
        assert!(
            writer
                .flush()
                .unwrap_err()
                .to_string()
                .contains("test failure")
        );
        let panicking = Writer::new(|_, _| panic!("isolated writer failure"));
        panicking.submit("settings".into(), vec![]).unwrap();
        assert!(
            panicking
                .flush()
                .unwrap_err()
                .to_string()
                .contains("worker failed")
        );
    }
}
