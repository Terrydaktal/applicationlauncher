//! Optional privileged Firefox bridge: bounded private snapshots, never session
//! history or page contents. All filesystem/proc work stays off the GUI thread.
use crate::models::{PactlSinkInput, WindowAudioCache, WindowInfo};
use eframe::egui;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const MAX_AGE: Duration = Duration::from_secs(12);
const MAX_BYTES: u64 = 512 * 1024;
pub(crate) const FIREFOX_WORKER_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct FirefoxAudioWindow {
    pub id: String,
    pub title: String,
    pub audible: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FirefoxAudioProcess {
    pub pid: i32,
    pub start_ticks: String,
    pub windows: Vec<FirefoxAudioWindow>,
}

#[derive(Deserialize)]
struct WireSnapshot {
    schema: u32,
    pid: i32,
    start_ticks: String,
    written_at_ms: u64,
    complete: bool,
    windows: Vec<FirefoxAudioWindow>,
}

pub(crate) struct FirefoxAudioUpdate {
    pub processes: Arc<Vec<FirefoxAudioProcess>>,
    pub captured_at: Instant,
}

#[derive(Default)]
pub(crate) struct FirefoxAudioInbox(Mutex<Option<FirefoxAudioUpdate>>);

impl FirefoxAudioInbox {
    pub(crate) fn take_latest(&self, ctx: &egui::Context) -> Option<FirefoxAudioUpdate> {
        match self.0.try_lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => {
                ctx.request_repaint_after(POLL_INTERVAL);
                None
            }
        }
    }
}

pub(crate) fn start_firefox_audio_bridge(ctx: egui::Context) -> Arc<FirefoxAudioInbox> {
    let inbox = Arc::new(FirefoxAudioInbox::default());
    let target = Arc::downgrade(&inbox);
    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("applicationlauncher-firefox-audio"));
    applicationlauncher::observability::spawn_named("firefox-audio-bridge", move |worker| {
        let mut previous = Arc::new(Vec::new());
        let notify = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        let notify = (notify >= 0).then(|| unsafe { OwnedFd::from_raw_fd(notify) });
        // One latest-state slot; polling ends when the GUI drops its receiver.
        while target.strong_count() > 0 {
            worker.set_state("reading-firefox-audio-ownership");
            let current = directory
                .as_ref()
                .map(|path| read_processes(path))
                .unwrap_or_default();
            let changed = current != *previous;
            if changed {
                previous = Arc::new(current);
            }
            if let Some(inbox) = target.upgrade() {
                if let Ok(mut slot) = inbox.0.lock() {
                    *slot = Some(FirefoxAudioUpdate {
                        processes: Arc::clone(&previous),
                        captured_at: Instant::now(),
                    });
                }
                if changed {
                    ctx.request_repaint();
                }
            }
            worker.set_state("waiting-for-firefox-audio-change");
            wait_for_snapshot(notify.as_ref(), directory.as_deref());
        }
    });
    inbox
}

fn wait_for_snapshot(notify: Option<&OwnedFd>, directory: Option<&Path>) {
    if let (Some(fd), Some(directory)) = (notify, directory)
        && let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes())
    {
        // Re-adding the same watch is idempotent, and also repairs directory
        // deletion/recreation. Overflow simply causes another latest-state read.
        unsafe {
            libc::inotify_add_watch(
                fd.as_raw_fd(),
                path.as_ptr(),
                libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE | libc::IN_DELETE | libc::IN_DELETE_SELF,
            );
            let mut poll = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if libc::poll(&mut poll, 1, POLL_INTERVAL.as_millis() as i32) > 0 {
                let mut events = [0u8; 8192];
                libc::read(fd.as_raw_fd(), events.as_mut_ptr().cast(), events.len());
            }
        }
    } else {
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn private(metadata: &fs::Metadata, uid: u32) -> bool {
    metadata.uid() == uid && metadata.mode() & 0o077 == 0
}

fn read_processes(directory: &Path) -> Vec<FirefoxAudioProcess> {
    let uid = unsafe { libc::geteuid() };
    let Ok(metadata) = fs::symlink_metadata(directory) else {
        return Vec::new();
    };
    if !metadata.is_dir() || !private(&metadata, uid) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let mut result = Vec::new();
    for entry in entries.take(129).flatten() {
        if result.len() >= 32 {
            return Vec::new();
        }
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|s| s.strip_prefix("firefox-"))
            .and_then(|s| s.strip_suffix(".json"))
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|pid| *pid > 0)
        else {
            continue;
        };
        if name != format!("firefox-{pid}.json").as_str() {
            continue;
        }
        if let Some(process) = read_process(&entry.path(), pid, uid, now) {
            result.push(process);
        }
    }
    result.sort_by_key(|process| process.pid);
    result
}

fn read_process(path: &Path, pid: i32, uid: u32, now: SystemTime) -> Option<FirefoxAudioProcess> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || !private(&metadata, uid)
        || metadata.len() > MAX_BYTES
        || now.duration_since(metadata.modified().ok()?).ok()? > MAX_AGE
    {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let wire: WireSnapshot = serde_json::from_slice(&bytes).ok()?;
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let start = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    let now_ms = now.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis() as u64;
    validate_snapshot(wire, pid, start, now_ms)
}

fn validate_snapshot(
    wire: WireSnapshot,
    pid: i32,
    start: &str,
    now_ms: u64,
) -> Option<FirefoxAudioProcess> {
    if wire.schema != 1
        || !wire.complete
        || wire.pid != pid
        || pid <= 0
        || wire.start_ticks != start
        || wire.windows.len() > 128
        || now_ms.checked_sub(wire.written_at_ms)? > MAX_AGE.as_millis() as u64
    {
        return None;
    }
    let mut ids = HashSet::new();
    let mut total = 0;
    for win in &wire.windows {
        total += win.audible.len();
        if win.id.is_empty()
            || win.id.len() > 128
            || !ids.insert(&win.id)
            || win.title.len() > 4096
            || total > 512
            || win.audible.iter().any(|s| s.len() > 4096)
        {
            return None;
        }
    }
    Some(FirefoxAudioProcess {
        pid,
        start_ticks: wire.start_ticks,
        windows: wire.windows,
    })
}

pub(crate) fn apply_firefox_attribution(
    cache: &mut WindowAudioCache,
    windows: &[WindowInfo],
    sinks: &[PactlSinkInput],
    processes: &[FirefoxAudioProcess],
) {
    for process in processes {
        for sink in sinks.iter().filter(|sink| {
            sink.properties
                .get("application.process.id")
                .and_then(|pid| pid.parse::<i32>().ok())
                == Some(process.pid)
        }) {
            // Authoritative pause/mute data overrides a PID-only assignment too.
            for indices in cache.visualization_sinks.values_mut() {
                indices.retain(|index| *index != sink.index);
            }
            if !super::sink_input_can_visualize(sink) {
                continue;
            }
            let audible: Vec<_> = process
                .windows
                .iter()
                .filter(|win| !win.audible.is_empty())
                .collect();
            let name = sink.properties.get("media.name");
            let candidates: Vec<_> = audible
                .iter()
                .filter(|win| {
                    audible.len() == 1
                        || name.is_some_and(|name| !name.is_empty() && win.audible.contains(name))
                })
                .collect();
            let [owner] = candidates.as_slice() else {
                continue;
            };
            if process
                .windows
                .iter()
                .filter(|win| win.title == owner.title)
                .count()
                != 1
            {
                continue;
            }
            let mut matches = windows.iter().filter(|win| {
                win.pid == Some(process.pid)
                    && (if win.raw_title.is_empty() {
                        &win.title
                    } else {
                        &win.raw_title
                    }) == &owner.title
            });
            let Some(window) = matches.next() else {
                continue;
            };
            if matches.next().is_some() {
                continue;
            }
            cache
                .visualization_sinks
                .entry(window.id.clone())
                .or_default()
                .push(sink.index);
        }
    }
    cache
        .visualization_sinks
        .retain(|_, indices| !indices.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{
        build_window_audio_cache,
        tests::{firefox_sink, firefox_window},
    };
    use std::os::unix::fs::PermissionsExt;

    fn process() -> FirefoxAudioProcess {
        FirefoxAudioProcess {
            pid: 100,
            start_ticks: "1234".into(),
            windows: vec![
                FirefoxAudioWindow {
                    id: "1".into(),
                    title: "Work - Firefox".into(),
                    audible: vec![],
                },
                FirefoxAudioWindow {
                    id: "2".into(),
                    title: "Other selected tab - Firefox".into(),
                    audible: vec!["Music - YouTube".into()],
                },
            ],
        }
    }

    fn routed(process: FirefoxAudioProcess, sinks: &[PactlSinkInput]) -> WindowAudioCache {
        let windows = [
            firefox_window("work", 100, "Work - Firefox"),
            firefox_window("music", 100, "Other selected tab - Firefox"),
        ];
        let mut cache = build_window_audio_cache(&windows, sinks);
        apply_firefox_attribution(&mut cache, &windows, sinks, &[process]);
        cache
    }

    #[test]
    fn background_tab_routes_to_one_window_even_when_another_tab_is_selected() {
        let cache = routed(process(), &[firefox_sink(7, 100)]);
        assert_eq!(cache.visualization_sinks.get("music"), Some(&vec![7]));
        assert!(!cache.visualization_sinks.contains_key("work"));
        assert_eq!(
            cache.sink_matches.len(),
            2,
            "volume controls remain process-wide"
        );
    }

    #[test]
    fn pause_and_resume_do_not_leave_the_previous_window_marked() {
        let mut state = process();
        state.windows[1].audible.clear();
        assert!(
            routed(state.clone(), &[firefox_sink(7, 100)])
                .visualization_sinks
                .is_empty()
        );
        state.windows[0].audible.push("Music - YouTube".into());
        let cache = routed(state, &[firefox_sink(7, 100)]);
        assert_eq!(cache.visualization_sinks.get("work"), Some(&vec![7]));
        assert!(!cache.visualization_sinks.contains_key("music"));
    }

    #[test]
    fn simultaneous_windows_use_exact_tab_names_not_a_shared_process_guess() {
        let mut state = process();
        state.windows[0].audible.push("Different recording".into());
        let mut other = firefox_sink(8, 100);
        other
            .properties
            .insert("media.name".into(), "Different recording".into());
        let cache = routed(state.clone(), &[firefox_sink(7, 100), other]);
        assert_eq!(cache.visualization_sinks.get("work"), Some(&vec![8]));
        assert_eq!(cache.visualization_sinks.get("music"), Some(&vec![7]));
        state.windows[0].audible.push("Music - YouTube".into());
        assert!(
            routed(state, &[firefox_sink(7, 100)])
                .visualization_sinks
                .is_empty()
        );
    }

    #[test]
    fn duplicate_window_titles_and_unmatched_titles_are_not_guessed() {
        let mut state = process();
        state.windows[0].title = state.windows[1].title.clone();
        assert!(
            routed(state, &[firefox_sink(7, 100)])
                .visualization_sinks
                .is_empty()
        );
        let mut state = process();
        state.windows[1].title = "Old window title".into();
        assert!(
            routed(state, &[firefox_sink(7, 100)])
                .visualization_sinks
                .is_empty()
        );
    }

    #[test]
    fn mute_and_foreign_pid_never_become_bridge_playback() {
        let mut sink = firefox_sink(7, 100);
        sink.mute = true;
        assert!(routed(process(), &[sink]).visualization_sinks.is_empty());
        assert!(
            routed(process(), &[firefox_sink(7, 101)])
                .visualization_sinks
                .is_empty()
        );
    }

    fn wire() -> WireSnapshot {
        WireSnapshot {
            schema: 1,
            pid: 100,
            start_ticks: "1234".into(),
            written_at_ms: 50_000,
            complete: true,
            windows: process().windows,
        }
    }

    #[test]
    fn stale_incomplete_future_reused_pid_and_unknown_versions_fail_closed() {
        assert!(validate_snapshot(wire(), 100, "1234", 50_001).is_some());
        assert!(validate_snapshot(wire(), 100, "1234", 62_001).is_none());
        assert!(validate_snapshot(wire(), 100, "1234", 49_999).is_none());
        assert!(validate_snapshot(wire(), 100, "9999", 50_001).is_none());
        assert!(validate_snapshot(wire(), 101, "1234", 50_001).is_none());
        let mut value = wire();
        value.complete = false;
        assert!(validate_snapshot(value, 100, "1234", 50_001).is_none());
        let mut value = wire();
        value.schema = 2;
        assert!(validate_snapshot(value, 100, "1234", 50_001).is_none());
        let mut value = wire();
        value.windows[1].id = "1".into();
        assert!(validate_snapshot(value, 100, "1234", 50_001).is_none());
    }

    #[test]
    fn snapshot_reader_rejects_public_files_symlinks_and_deleted_processes() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("al-firefox-reader-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let pid = std::process::id() as i32;
        let stat = fs::read_to_string("/proc/self/stat").unwrap();
        let start = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap();
        let path = root.join(format!("firefox-{pid}.json"));
        let data = serde_json::json!({"schema": 1, "pid": pid, "start_ticks": start,
            "written_at_ms": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis() as u64,
            "complete": true, "windows": []});
        fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_processes(&root).len(), 1);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_processes(&root).is_empty());
        fs::rename(&path, root.join("target")).unwrap();
        std::os::unix::fs::symlink(root.join("target"), &path).unwrap();
        assert!(read_processes(&root).is_empty());
        fs::remove_file(&path).unwrap();
        fs::write(
            root.join("firefox-2147483647.json"),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
        assert!(read_processes(&root).is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
