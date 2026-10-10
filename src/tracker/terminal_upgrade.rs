use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{TerminalTabRestore, TrackedWindow};

const MAX_JOBS: usize = 16;
const MAX_WINDOWS: usize = 512;
const MAX_TABS: usize = 32;
const MAX_PROCESSES: usize = 512;
const PREVIEW_LIFETIME: Duration = Duration::from_secs(300);
const WINDOW_TIMEOUT: Duration = Duration::from_secs(30);
const JOB_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalUpgradePhase {
    #[default]
    Preparing,
    Ready,
    Running,
    Complete,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalUpgradeWindow {
    pub window_id: String,
    pub title: String,
    pub tabs: Vec<String>,
    pub eligible: bool,
    pub recommended: bool,
    pub preserves_processes: bool,
    pub executable: String,
    pub build_id: Option<String>,
    pub sha256: String,
    pub status: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TerminalUpgradeReport {
    pub operation_id: String,
    pub phase: TerminalUpgradePhase,
    pub backup_snapshot_id: Option<i64>,
    pub windows: Vec<TerminalUpgradeWindow>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    pid: i32,
    name: String,
    parent: i32,
    group: i32,
    start: u64,
    executable: PathBuf,
    arguments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TabRecord {
    owner: i32,
    window_uuid: String,
    tab_uuid: String,
    title: String,
    cwd: String,
    active: bool,
    child: i32,
    foreground: i32,
    pty: String,
}

#[derive(Clone)]
struct CapturedTab {
    record: TabRecord,
    process: ProcessIdentity,
    restore: TerminalTabRestore,
    busy: bool,
}

struct PinnedExecutable {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    digest: String,
    build_id: Option<String>,
}

impl PinnedExecutable {
    fn open(path: &Path) -> Result<Self, String> {
        let path = std::fs::canonicalize(path).map_err(|err| err.to_string())?;
        let file = File::open(&path).map_err(|err| err.to_string())?;
        let metadata = file.metadata().map_err(|err| err.to_string())?;
        if !metadata.is_file() || metadata.mode() & 0o111 == 0 || metadata.len() > 512 * 1024 * 1024
        {
            return Err("The replacement is not a bounded executable file".into());
        }
        let pinned = PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            file.as_raw_fd()
        ));
        let mut reader = File::open(&pinned).map_err(|err| err.to_string())?;
        let mut magic = [0; 4];
        reader
            .read_exact(&mut magic)
            .map_err(|err| err.to_string())?;
        if magic != *b"\x7fELF" {
            return Err(
                "The replacement must be the actual terminal ELF binary, not a wrapper".into(),
            );
        }
        let mut digest = Sha256::new();
        digest.update(magic);
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer).map_err(|err| err.to_string())?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        let build_id = crate::process::executable_path("readelf").and_then(|readelf| {
            let mut command = Command::new(readelf);
            command.args(["-n"]).arg(&pinned).env("LC_ALL", "C");
            let output =
                crate::process::output_with_timeout(command, Duration::from_secs(1)).ok()?;
            String::from_utf8(output.stdout)
                .ok()?
                .lines()
                .find_map(|line| {
                    let id = line.trim().strip_prefix("Build ID: ")?;
                    (id.len() <= 128 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
                        .then(|| id.to_string())
                })
        });
        let result = Self {
            file,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            digest: format!("{:x}", digest.finalize()),
            build_id,
        };
        result.validate_current()?;
        Ok(result)
    }

    fn pinned_path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.file.as_raw_fd()
        ))
    }

    fn validate_current(&self) -> Result<(), String> {
        let current = std::fs::metadata(&self.path).map_err(|err| err.to_string())?;
        let pinned = self.file.metadata().map_err(|err| err.to_string())?;
        if current.dev() != self.device
            || current.ino() != self.inode
            || current.len() != self.length
            || pinned.len() != self.length
            || (current.mtime(), current.mtime_nsec()) != self.modified
            || (pinned.mtime(), pinned.mtime_nsec()) != self.modified
        {
            return Err("The terminal binary changed after preview; refresh the preview".into());
        }
        Ok(())
    }

    fn matches_process(&self, pid: i32) -> bool {
        std::fs::metadata(format!("/proc/{pid}/exe"))
            .is_ok_and(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode)
    }
}

#[derive(Clone)]
pub(super) struct UpgradeTarget {
    pub window: TrackedWindow,
    owner: ProcessIdentity,
    tabs: Vec<CapturedTab>,
    executable: Arc<PinnedExecutable>,
}

impl UpgradeTarget {
    pub(super) fn restore_tabs(&self) -> Vec<TerminalTabRestore> {
        self.tabs.iter().map(|tab| tab.restore.clone()).collect()
    }

    fn preserves_processes(&self) -> bool {
        self.tabs.iter().all(|tab| tab.restore.kind == "tmux")
    }
}

pub(super) struct PreparedUpgrade {
    targets: Vec<UpgradeTarget>,
    all_windows: Vec<TrackedWindow>,
}

struct Job {
    created: Instant,
    report: TerminalUpgradeReport,
    prepared: Option<PreparedUpgrade>,
}

#[derive(Default)]
pub(super) struct UpgradeRegistry {
    jobs: VecDeque<(String, Arc<Mutex<Job>>)>,
}

impl UpgradeRegistry {
    pub(super) fn begin(&mut self) -> (String, Option<UpgradePreparation>) {
        if let Some((id, _)) = self.jobs.iter().find(|(_, job)| {
            matches!(
                job.lock().unwrap().report.phase,
                TerminalUpgradePhase::Preparing | TerminalUpgradePhase::Running
            )
        }) {
            return (id.clone(), None);
        }
        // Keep compact historical reports, but only one actionable preview may
        // retain pinned executables and detailed process metadata.
        for (_, job) in &self.jobs {
            let mut job = job.lock().unwrap();
            if job.report.phase == TerminalUpgradePhase::Ready {
                job.prepared = None;
                job.report.phase = TerminalUpgradePhase::Failed;
                job.report.message = "Preview superseded; use the newest preview".into();
            }
        }
        while self.jobs.len() >= MAX_JOBS {
            self.jobs.pop_front();
        }
        let id = format!(
            "terminal-upgrade-{}-{}",
            super::now_ms(),
            crate::observability::next_operation_id()
        );
        let job = Arc::new(Mutex::new(Job {
            created: Instant::now(),
            report: TerminalUpgradeReport {
                operation_id: id.clone(),
                message: "Inspecting terminal windows and every tab...".into(),
                ..Default::default()
            },
            prepared: None,
        }));
        self.jobs.push_back((id.clone(), job.clone()));
        (id, Some(UpgradePreparation(job)))
    }

    pub(super) fn report(&self, id: &str) -> Result<TerminalUpgradeReport, String> {
        let job = self
            .jobs
            .iter()
            .find(|(key, _)| key == id)
            .ok_or("Terminal restart preview is unavailable; refresh it")?;
        Ok(job.1.lock().unwrap().report.clone())
    }

    pub(super) fn start(
        &self,
        id: &str,
        selected: &[String],
        confirmed: bool,
    ) -> Result<Option<UpgradeWork>, String> {
        if !confirmed || selected.is_empty() {
            return Err("Explicit confirmation and at least one terminal are required".into());
        }
        let job = self
            .jobs
            .iter()
            .find(|(key, _)| key == id)
            .ok_or("Terminal restart preview is unavailable; refresh it")?
            .1
            .clone();
        let mut state = job.lock().unwrap();
        if matches!(
            state.report.phase,
            TerminalUpgradePhase::Running | TerminalUpgradePhase::Complete
        ) {
            return Ok(None);
        }
        if state.report.phase != TerminalUpgradePhase::Ready
            || state.created.elapsed() >= PREVIEW_LIFETIME
        {
            return Err(
                "The terminal restart preview is not ready or has expired; refresh it".into(),
            );
        }
        if self.jobs.iter().any(|(_, other)| {
            !Arc::ptr_eq(other, &job)
                && other.lock().unwrap().report.phase == TerminalUpgradePhase::Running
        }) {
            return Err("A terminal restart is already running".into());
        }
        let ids = selected.iter().collect::<HashSet<_>>();
        if ids.len() != selected.len()
            || ids.iter().any(|id| {
                !state
                    .report
                    .windows
                    .iter()
                    .any(|window| &window.window_id == *id && window.eligible)
            })
        {
            return Err("The selection contains duplicate, unknown or unsupported windows".into());
        }
        let mut prepared = state
            .prepared
            .take()
            .ok_or("The preview has no verified terminal data")?;
        prepared
            .targets
            .retain(|target| ids.contains(&target.window.id));
        state.report.phase = TerminalUpgradePhase::Running;
        state.report.message = "Saving a durable backup before changing any windows...".into();
        for window in &mut state.report.windows {
            if ids.contains(&window.window_id) {
                window.status = "pending".into();
            } else {
                window.status = "skipped".into();
            }
        }
        drop(state);
        Ok(Some(UpgradeWork {
            job,
            prepared: Some(prepared),
            completed: false,
        }))
    }
}

pub(super) struct UpgradePreparation(Arc<Mutex<Job>>);

impl UpgradePreparation {
    pub(super) fn prepare(self, windows: Vec<TrackedWindow>) {
        let result = capture_upgrade(&windows);
        let mut job = self.0.lock().unwrap();
        match result {
            Ok((prepared, preview)) => {
                job.prepared = Some(prepared);
                job.report.windows = preview;
                job.report.phase = TerminalUpgradePhase::Ready;
                job.report.message =
                    "No windows have been changed. Select terminals and confirm to restart them."
                        .into();
            }
            Err(error) => {
                job.report.phase = TerminalUpgradePhase::Failed;
                job.report.message = error;
            }
        }
    }
}

impl Drop for UpgradePreparation {
    fn drop(&mut self) {
        let mut job = self.0.lock().unwrap();
        if job.report.phase == TerminalUpgradePhase::Preparing {
            job.report.phase = TerminalUpgradePhase::Failed;
            job.report.message =
                "Terminal inspection did not complete. No windows were changed.".into();
        }
    }
}

pub(super) trait UpgradeBackend {
    fn windows(&self) -> Vec<TrackedWindow>;
    fn backup(
        &self,
        all_windows: &[TrackedWindow],
        targets: &[UpgradeTarget],
    ) -> Result<i64, String>;
    fn preflight(&self, target: &UpgradeTarget) -> Result<(), String>;
    fn launch_and_verify(&self, target: &UpgradeTarget) -> Result<String, String>;
    fn close_and_wait(&self, target: &UpgradeTarget) -> Result<(), String>;
    fn finish_layout(&self, windows: &[(TrackedWindow, String)]) -> Result<(), String>;
}

pub(super) struct UpgradeWork {
    job: Arc<Mutex<Job>>,
    prepared: Option<PreparedUpgrade>,
    completed: bool,
}

impl UpgradeWork {
    pub(super) fn window_ids(&self) -> Vec<String> {
        self.prepared
            .as_ref()
            .map(|prepared| {
                prepared
                    .targets
                    .iter()
                    .map(|target| target.window.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn run(mut self, backend: &impl UpgradeBackend) {
        let prepared = self.prepared.take().unwrap();
        let snapshot = match backend.backup(&prepared.all_windows, &prepared.targets) {
            Ok(id) => id,
            Err(error) => {
                let mut job = self.job.lock().unwrap();
                job.report.phase = TerminalUpgradePhase::Failed;
                job.report.message = format!("Backup failed; no windows were changed: {error}");
                self.completed = true;
                return;
            }
        };
        self.job.lock().unwrap().report.backup_snapshot_id = Some(snapshot);
        let deadline = Instant::now() + JOB_TIMEOUT;
        let mut replacements = HashMap::new();
        // Each target makes at most one close and one launch attempt. Failures are
        // reported, never force-killed or automatically retried.
        for target in &prepared.targets {
            self.update(
                &target.window.id,
                "running",
                "Checking fresh terminal identity...",
            );
            let result = if Instant::now() >= deadline {
                Err("The restart time budget expired; this window was not touched".into())
            } else {
                replace_one(backend, target)
            };
            match result {
                Ok(id) => {
                    replacements.insert(target.window.id.clone(), id);
                    self.update(
                        &target.window.id,
                        "restarted",
                        "Replacement binary, tabs and placement verified",
                    );
                }
                Err(error) => self.update(&target.window.id, "failed", &error),
            }
        }
        let current_ids = backend
            .windows()
            .into_iter()
            .map(|window| window.id)
            .collect::<HashSet<_>>();
        let layout = prepared
            .all_windows
            .iter()
            .filter_map(|window| {
                let id = replacements
                    .get(&window.id)
                    .cloned()
                    .unwrap_or_else(|| window.id.clone());
                current_ids.contains(&id).then(|| (window.clone(), id))
            })
            .collect::<Vec<_>>();
        let layout_error = if replacements.is_empty() {
            None
        } else {
            backend.finish_layout(&layout).err()
        };
        let mut job = self.job.lock().unwrap();
        let success = job
            .report
            .windows
            .iter()
            .filter(|window| window.status == "restarted")
            .count();
        let failed = job
            .report
            .windows
            .iter()
            .filter(|window| window.status == "failed")
            .count();
        job.report.phase = TerminalUpgradePhase::Complete;
        job.report.message = format!(
            "Restarted {success} terminals; {failed} failed. Backup #{snapshot} is retained in Saved sessions.{}",
            layout_error
                .map(|err| format!(" Stacking/focus restoration failed: {err}"))
                .unwrap_or_default()
        );
        self.completed = true;
    }

    fn update(&self, id: &str, status: &str, message: &str) {
        let mut job = self.job.lock().unwrap();
        if let Some(window) = job
            .report
            .windows
            .iter_mut()
            .find(|window| window.window_id == id)
        {
            window.status = status.into();
            window.message = message.into();
        }
        job.report.message = message.into();
    }
}

impl Drop for UpgradeWork {
    fn drop(&mut self) {
        if !self.completed {
            let mut job = self.job.lock().unwrap();
            job.report.phase = TerminalUpgradePhase::Failed;
            job.report.message = "Restart worker stopped unexpectedly; no automatic retry was scheduled. Any saved backup is retained.".into();
        }
    }
}

fn replace_one(backend: &impl UpgradeBackend, target: &UpgradeTarget) -> Result<String, String> {
    backend.preflight(target)?;
    if target.preserves_processes() {
        let replacement = backend.launch_and_verify(target)?;
        // tmux's original client remains until the replacement has attached to
        // the exact same server/session and its geometry has been verified.
        backend.preflight(target)?;
        backend.close_and_wait(target).map_err(|error| format!("Replacement {replacement} is verified, but the original did not close; both may remain open: {error}"))?;
        Ok(replacement)
    } else {
        backend.close_and_wait(target)?;
        backend.launch_and_verify(target).map_err(|error| {
            format!("The original closed but replacement failed; use the saved backup: {error}")
        })
    }
}

pub(super) fn preflight_target(
    target: &UpgradeTarget,
    windows: &[TrackedWindow],
) -> Result<(), String> {
    target.executable.validate_current()?;
    if process_identity(target.owner.pid).as_ref() != Some(&target.owner) {
        return Err("The terminal owner process changed; original left untouched".into());
    }
    let window = windows
        .iter()
        .find(|window| window.id == target.window.id)
        .ok_or("The original window is no longer open")?;
    if window.pid != target.window.pid
        || window.class != target.window.class
        || !same_placement(window, &target.window)
    {
        return Err("The window identity or placement changed after preview; refresh it".into());
    }
    let records = terminal_records()?;
    let fresh = capture_tabs(window, &records)?;
    if fresh.len() != target.tabs.len()
        || fresh.iter().zip(&target.tabs).any(|(fresh, saved)| {
            !same_tab_identity(&fresh.record, &saved.record)
                || fresh.process != saved.process
                || fresh.restore != saved.restore
                || (fresh.busy && !saved.busy)
        })
    {
        return Err(
            "Terminal tabs, programs or activity changed after preview; original left untouched"
                .into(),
        );
    }
    let outputs = super::restore::current_output_geometries();
    if super::restore::layout_target(&target.window, &outputs)
        .adjustment
        .is_some()
    {
        return Err("Monitor topology or geometry is unavailable/changed; exact placement cannot be guaranteed".into());
    }
    super::restore::terminal_tab_arguments(&target.restore_tabs()).map(|_| ())
}

fn same_placement(left: &TrackedWindow, right: &TrackedWindow) -> bool {
    (left.x, left.y, left.width, left.height) == (right.x, right.y, right.width, right.height)
        && left.output == right.output
        && left.output_geometry == right.output_geometry
        && left.minimized == right.minimized
        && left.normal_geometry == right.normal_geometry
        && left.maximized == right.maximized
        && left.maximized_horizontally == right.maximized_horizontally
        && left.maximized_vertically == right.maximized_vertically
        && left.fullscreen == right.fullscreen
        && left.desktop == right.desktop
        && left.on_all_desktops == right.on_all_desktops
        && left.activities == right.activities
        && left.keep_above == right.keep_above
        && left.keep_below == right.keep_below
        && left.no_border == right.no_border
        && left.shaded == right.shaded
        && left.skip_pager == right.skip_pager
}

pub(super) fn launch_replacement(
    target: &UpgradeTarget,
    windows: impl Fn() -> Vec<TrackedWindow>,
) -> Result<String, String> {
    let tabs = target.restore_tabs();
    let arguments = super::restore::terminal_tab_arguments(&tabs)?;
    let baseline = windows()
        .into_iter()
        .map(|window| window.id)
        .collect::<HashSet<_>>();
    let command = pinned_launch_command(&target.executable, arguments)?;
    // Always use an independent application scope: daemon/GUI exit must never
    // tear down the replacement or its child programs.
    let pid =
        crate::process::spawn_and_reap_with_pid(command).map_err(|err| err.to_string())? as i32;
    let deadline = Instant::now() + WINDOW_TIMEOUT;
    let outputs = super::restore::current_output_geometries();
    let layout = super::restore::layout_target(&target.window, &outputs);
    if layout.adjustment.is_some() {
        return Err("Monitor topology changed; exact placement cannot be guaranteed".into());
    }
    let mut applied = None;
    let mut attempts = 0;
    let mut verified_at = None;
    while Instant::now() < deadline {
        let current = windows();
        let candidates = current
            .iter()
            .filter(|window| !baseline.contains(&window.id) && window.pid == pid)
            .collect::<Vec<_>>();
        if let [window] = candidates.as_slice() {
            if !target.executable.matches_process(window.pid) {
                return Err("The replacement is not running the verified terminal binary".into());
            }
            if applied.as_deref() != Some(window.id.as_str())
                || (verified_at.is_none() && attempts < 3)
            {
                super::restore::apply_layout_once(&window.id, &target.window, &layout)?;
                applied = Some(window.id.clone());
                attempts += 1;
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            if super::restore::verify_layout(window, &target.window, &layout).is_ok()
                && verify_replacement_tabs(window, &tabs).is_ok()
            {
                if verified_at.is_some_and(|at: Instant| at.elapsed() >= Duration::from_millis(350))
                {
                    return Ok(window.id.clone());
                }
                verified_at.get_or_insert_with(Instant::now);
            } else {
                verified_at = None;
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Err("Replacement did not produce a uniquely identified, correctly placed terminal with all expected tabs before the deadline; no window was force-killed".into())
}

fn pinned_launch_command(
    executable: &PinnedExecutable,
    arguments: Vec<std::ffi::OsString>,
) -> Result<Command, String> {
    let bash = crate::process::executable_path("bash")
        .ok_or("bash is required to preserve the terminal's argv[0]")?;
    let mut command = Command::new(bash);
    // systemd-run must receive the parent's pinned FD path, not /proc/self/fd.
    // Preserve argv[0] as well: GTK derives its program/application identity from it.
    command
        .args(["-c", "exec -a \"$1\" \"$2\" \"${@:3}\"", "terminal-upgrade"])
        .arg(&executable.path)
        .arg(executable.pinned_path())
        .args(arguments);
    Ok(command)
}

pub(super) fn close_original(
    target: &UpgradeTarget,
    windows: impl Fn() -> Vec<TrackedWindow>,
) -> Result<(), String> {
    // Repeat the complete identity check immediately before the only close action.
    preflight_target(target, &windows())?;
    let connection = session_connection()?;
    let proxy = zbus::blocking::Proxy::new(&connection, "org.kde.KWin", "/KWin", "org.kde.KWin")
        .map_err(|err| err.to_string())?;
    let info: HashMap<String, zbus::zvariant::OwnedValue> = proxy
        .call("getWindowInfo", &(target.window.id.as_str(),))
        .map_err(|err| err.to_string())?;
    if value_i32(&info, "pid") != Some(target.owner.pid) {
        return Err(
            "KWin no longer identifies this window as the captured terminal process".into(),
        );
    }
    if ["x", "y", "width", "height"]
        .into_iter()
        .map(|key| value_i32(&info, key))
        .collect::<Vec<_>>()
        != [
            target.window.x,
            target.window.y,
            target.window.width,
            target.window.height,
        ]
        .map(Some)
    {
        return Err("KWin reports changed or unavailable geometry; original left untouched".into());
    }
    let mut command = Command::new(crate::process::kdotool_path());
    command.args(["windowclose", &target.window.id]);
    let status = crate::process::status_with_timeout(command, Duration::from_secs(3))
        .map_err(|err| err.to_string())?;
    if !status.success() {
        return Err(format!("KWin rejected the close request ({status})"));
    }
    let deadline = Instant::now() + WINDOW_TIMEOUT;
    let mut window_closed = false;
    while Instant::now() < deadline {
        // Query KWin directly as well as the feed; a delayed Removed signal must
        // not make us close or relaunch the window a second time.
        let info: HashMap<String, zbus::zvariant::OwnedValue> = proxy
            .call("getWindowInfo", &(target.window.id.as_str(),))
            .map_err(|err| err.to_string())?;
        window_closed = info.is_empty();
        if window_closed
            && target
                .tabs
                .iter()
                .filter(|tab| tab.restore.kind != "tmux")
                .all(|tab| original_program_stopped(&tab.process))
        {
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    if window_closed {
        return Err("The original window closed, but a non-tmux program is still running or could not be verified as stopped; no replacement was launched. Use the saved backup after resolving it".into());
    }
    Err("The original window did not close. It may be waiting for its own confirmation; no force-kill or retry was performed".into())
}

fn original_program_stopped(process: &ProcessIdentity) -> bool {
    let directory = PathBuf::from(format!("/proc/{}", process.pid));
    match bounded_read(directory.join("stat"), 16 * 1024) {
        Some(stat) => exited_process_instance(&stat, process.start).unwrap_or(false),
        None => !directory.exists(),
    }
}

fn exited_process_instance(stat: &[u8], saved_start: u64) -> Option<bool> {
    let stat = std::str::from_utf8(stat).ok()?;
    let fields = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let start: u64 = fields.get(19)?.parse().ok()?;
    Some(start != saved_start || matches!(*fields.first()?, "Z" | "X"))
}

fn capture_upgrade(
    windows: &[TrackedWindow],
) -> Result<(PreparedUpgrade, Vec<TerminalUpgradeWindow>), String> {
    if windows.len() > MAX_WINDOWS {
        return Err("Too many windows for a bounded terminal restart".into());
    }
    let records = terminal_records()?;
    let mut targets = Vec::new();
    let mut preview = Vec::new();
    let mut executables = HashMap::<PathBuf, Arc<PinnedExecutable>>::new();
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut candidates = windows
        .iter()
        .filter(|window| {
            super::app_key(window).contains("xfce4-terminal")
                || window.class.to_lowercase().contains("xfce4-terminal")
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|window| window.stacking_order);
    for window in candidates {
        let mut row = TerminalUpgradeWindow {
            window_id: window.id.clone(),
            title: window.title.clone(),
            tabs: Vec::new(),
            eligible: false,
            recommended: false,
            preserves_processes: false,
            executable: String::new(),
            build_id: None,
            sha256: String::new(),
            status: "unavailable".into(),
            message: String::new(),
        };
        let result = (|| {
            if Instant::now() >= deadline {
                return Err("Inspection time budget expired; refresh the preview".into());
            }
            if window.width <= 0 || window.height <= 0 {
                return Err("Window geometry is unavailable".into());
            }
            let owner =
                process_identity(window.pid).ok_or("The terminal process is unavailable")?;
            if executable_name(&owner.executable) != "xfce4-terminal" {
                return Err("The window PID is not a verified XFCE4 Terminal process".into());
            }
            let path = PathBuf::from(
                owner
                    .executable
                    .to_string_lossy()
                    .trim_end_matches(" (deleted)"),
            );
            let executable = match executables.get(&path) {
                Some(executable) => executable.clone(),
                None => {
                    let executable = Arc::new(PinnedExecutable::open(&path)?);
                    executables.insert(path, executable.clone());
                    executable
                }
            };
            row.executable = executable.path.to_string_lossy().into_owned();
            row.build_id = executable.build_id.clone();
            row.sha256 = executable.digest.clone();
            let tabs = capture_tabs(window, &records)?;
            row.tabs = tabs
                .iter()
                .map(|tab| {
                    format!(
                        "{}{} - {}{}",
                        tab.restore.kind,
                        tab.restore
                            .tmux_session
                            .as_ref()
                            .map(|session| format!(": {}", session.session_name))
                            .unwrap_or_default(),
                        tab.restore.cwd,
                        if tab.restore.active { " (active)" } else { "" }
                    )
                })
                .collect();
            super::restore::terminal_tab_arguments(
                &tabs
                    .iter()
                    .map(|tab| tab.restore.clone())
                    .collect::<Vec<_>>(),
            )?;
            row.preserves_processes = tabs.iter().all(|tab| tab.restore.kind == "tmux");
            row.recommended = row.preserves_processes || !tabs.iter().any(|tab| tab.busy);
            row.message = if row.preserves_processes {
                "Reattach every tmux tab; running programs remain alive".into()
            } else if !row.recommended {
                "Activity detected: not selected by default. Confirming restart interrupts running programs and loses terminal scrollback/shell state".into()
            } else {
                "Programs will restart; terminal scrollback and unsaved shell state will not survive".into()
            };
            Ok(UpgradeTarget {
                window: window.clone(),
                owner,
                tabs,
                executable,
            })
        })();
        match result {
            Ok(target) if target.executable.matches_process(target.owner.pid) => {
                row.recommended = false;
                row.status = "up_to_date".into();
                row.message = "Already running this binary; no restart is needed".into();
            }
            Ok(target) => {
                row.eligible = true;
                row.status = "ready".into();
                targets.push(target);
            }
            Err(error) => row.message = error,
        }
        preview.push(row);
    }
    Ok((
        PreparedUpgrade {
            targets,
            all_windows: windows.to_vec(),
        },
        preview,
    ))
}

fn capture_tabs(window: &TrackedWindow, records: &[TabRecord]) -> Result<Vec<CapturedTab>, String> {
    if records
        .iter()
        .any(|record| record.owner == window.pid && record.window_uuid.is_empty())
    {
        return Err(
            "This terminal server has incomplete window UUIDs; its windows cannot be safely mapped"
                .into(),
        );
    }
    let active = records
        .iter()
        .filter(|record| record.owner == window.pid && record.active)
        .collect::<Vec<_>>();
    let record = if active.len() == 1 {
        active[0]
    } else {
        let matches = active
            .into_iter()
            .filter(|record| stable_title(&record.title) == stable_title(&window.title))
            .collect::<Vec<_>>();
        let [record] = matches.as_slice() else {
            return Err("The terminal window cannot be uniquely mapped to all of its tabs".into());
        };
        *record
    };
    let tabs = records
        .iter()
        .filter(|tab| tab.owner == record.owner && tab.window_uuid == record.window_uuid)
        .collect::<Vec<_>>();
    if tabs.is_empty() || tabs.len() > MAX_TABS || tabs.iter().filter(|tab| tab.active).count() != 1
    {
        return Err("Incomplete or oversized terminal tab metadata".into());
    }
    if tabs.iter().any(|tab| {
        tab.tab_uuid.is_empty() || tab.child <= 1 || tab.foreground <= 1 || tab.pty.is_empty()
    }) {
        return Err(
            "One or more tabs lack a verified process/PTY identity; this entire window is skipped"
                .into(),
        );
    }
    let mut result = Vec::new();
    for record in tabs {
        let process = foreground_program(record)?;
        let kind = program_kind(&process);
        let cwd = std::fs::read_link(format!("/proc/{}/cwd", process.pid))
            .map_err(|err| err.to_string())?;
        let cwd = cwd
            .to_str()
            .filter(|_| cwd.is_dir())
            .ok_or("A tab's working directory is unavailable")?
            .to_string();
        let tmux_session = if kind == "tmux" {
            Some(
                super::tmux::capture_session(process.pid, &process.arguments, Some(&cwd))
                    .ok_or("The exact tmux server/session could not be identified")?,
            )
        } else {
            None
        };
        let codex_session_id = if kind == "codex" {
            Some(codex_session(process.pid, &process.arguments)?)
        } else {
            None
        };
        if ![
            "fish", "bash", "zsh", "tmux", "codex", "agy", "htop", "nvtop",
        ]
        .contains(&kind.as_str())
        {
            return Err(format!(
                "Tab runs unsupported program {kind:?}; SSH and arbitrary jobs are not safely replayable"
            ));
        }
        let busy = has_activity(&record.title)
            || ((kind == "codex" || kind == "agy")
                && process_children(process.pid).is_some_and(|children| !children.is_empty()));
        result.push(CapturedTab {
            record: record.clone(),
            process: process.clone(),
            busy,
            restore: TerminalTabRestore {
                cwd,
                kind,
                active: record.active,
                safe_arguments: super::codex_restore_arguments(&process.arguments),
                codex_session_id,
                tmux_session,
            },
        });
    }
    Ok(result)
}

fn verify_replacement_tabs(
    window: &TrackedWindow,
    expected: &[TerminalTabRestore],
) -> Result<(), String> {
    let actual = capture_tabs(window, &terminal_records()?)?;
    if actual.len() != expected.len()
        || actual
            .iter()
            .zip(expected)
            .any(|(actual, expected)| actual.restore != *expected)
    {
        return Err(
            "Replacement tabs are not running the expected programs, directories or exact sessions"
                .into(),
        );
    }
    Ok(())
}

fn has_activity(title: &str) -> bool {
    title
        .chars()
        .any(|character| ('\u{2800}'..='\u{28ff}').contains(&character))
        || title.to_lowercase().contains("action required")
}

fn stable_title(title: &str) -> String {
    title
        .replace("[ ! ] Action Required", "")
        .replace("[ . ] Action Required", "")
        .chars()
        .filter(|character| !('\u{2800}'..='\u{28ff}').contains(character))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn same_tab_identity(left: &TabRecord, right: &TabRecord) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.title = stable_title(&left.title);
    right.title = stable_title(&right.title);
    left == right
}

fn executable_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .trim_end_matches(" (deleted)")
        .to_string()
}

fn program_kind(process: &ProcessIdentity) -> String {
    for kind in ["codex", "agy"] {
        if super::terminal_process_matches(&process.name, process.executable.to_str(), kind) {
            return kind.into();
        }
    }
    executable_name(&process.executable)
}

fn bounded_read(path: impl AsRef<Path>, limit: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

fn process_identity(pid: i32) -> Option<ProcessIdentity> {
    let directory = PathBuf::from(format!("/proc/{pid}"));
    if pid <= 1 || std::fs::metadata(&directory).ok()?.uid() != rustix::process::getuid().as_raw() {
        return None;
    }
    let stat = String::from_utf8(bounded_read(directory.join("stat"), 16 * 1024)?).ok()?;
    let fields = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let arguments = bounded_read(directory.join("cmdline"), 64 * 1024)?
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8_lossy(argument).into_owned())
        .collect();
    Some(ProcessIdentity {
        pid,
        name: stat.split_once('(')?.1.rsplit_once(')')?.0.to_string(),
        parent: fields.get(1)?.parse().ok()?,
        group: fields.get(2)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
        executable: std::fs::read_link(directory.join("exe")).ok()?,
        arguments,
    })
}

fn process_children(pid: i32) -> Option<Vec<i32>> {
    String::from_utf8(bounded_read(
        format!("/proc/{pid}/task/{pid}/children"),
        16 * 1024,
    )?)
    .ok()?
    .split_whitespace()
    .map(str::parse)
    .collect::<Result<Vec<_>, _>>()
    .ok()
}

fn foreground_program(record: &TabRecord) -> Result<ProcessIdentity, String> {
    let mut pending = vec![record.child];
    let mut visited = HashSet::new();
    let mut programs = Vec::new();
    let mut shells = Vec::new();
    while let Some(pid) = pending.pop() {
        if visited.len() >= MAX_PROCESSES || !visited.insert(pid) {
            return Err("Ambiguous or oversized terminal process tree".into());
        }
        let process = process_identity(pid).ok_or("A terminal child process disappeared")?;
        let kind = program_kind(&process);
        if ["codex", "agy", "htop", "nvtop", "tmux", "ssh"].contains(&kind.as_str()) {
            if process.group == record.foreground {
                programs.push(process);
            } else {
                return Err(
                    "A tab has an additional background application; refusing to terminate it"
                        .into(),
                );
            }
            continue;
        }
        let children = process_children(pid).ok_or("A terminal process tree is unavailable")?;
        if children.is_empty() {
            if ["fish", "bash", "zsh"].contains(&kind.as_str())
                && process.group == record.foreground
            {
                shells.push(process);
            } else {
                return Err(format!(
                    "A tab contains unsupported or background program {kind:?}"
                ));
            }
        } else {
            for child in children {
                if process_identity(child).is_none_or(|child| child.parent != pid) {
                    return Err("A tab's process identity changed during inspection".into());
                }
                pending.push(child);
            }
        }
    }
    if let [process] = programs.as_slice() {
        return Ok(process.clone());
    }
    if programs.is_empty()
        && let [shell] = shells.as_slice()
    {
        return Ok(shell.clone());
    }
    Err("A tab's foreground program could not be uniquely identified".into())
}

fn codex_session(pid: i32, arguments: &[String]) -> Result<String, String> {
    let mut ids = HashSet::new();
    for argument in arguments.windows(2) {
        if argument[0] == "resume" && super::restore::valid_session_id(&argument[1]) {
            ids.insert(argument[1].clone());
        }
    }
    let descriptors =
        std::fs::read_dir(format!("/proc/{pid}/fd")).map_err(|err| err.to_string())?;
    for entry in descriptors.take(512).flatten() {
        let Ok(path) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            continue;
        }
        let Ok(file) = File::open(entry.path()) else {
            continue;
        };
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            continue;
        }
        let mut line = String::new();
        if BufReader::new(file.take(1024 * 1024))
            .read_line(&mut line)
            .is_err()
        {
            continue;
        }
        if let Some(id) = rollout_session_id(&line) {
            ids.insert(id);
        }
    }
    if ids.len() == 1 {
        return Ok(ids.into_iter().next().unwrap());
    }
    Err(
        "The exact Codex session is unavailable or ambiguous; refusing to guess with resume --last"
            .into(),
    )
}

fn rollout_session_id(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value["type"] != "session_meta" {
        return None;
    }
    let id = value["payload"]["id"].as_str()?;
    super::restore::valid_session_id(id).then(|| id.to_string())
}

fn session_connection() -> Result<zbus::blocking::Connection, String> {
    zbus::blocking::connection::Builder::session()
        .map_err(|err| err.to_string())?
        .method_timeout(Duration::from_secs(2))
        .build()
        .map_err(|err| err.to_string())
}

fn value_string(values: &HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> Option<String> {
    zbus::zvariant::Value::try_from(values.get(key)?)
        .ok()?
        .downcast_ref::<String>()
        .ok()
}

fn value_i32(values: &HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> Option<i32> {
    let value = zbus::zvariant::Value::try_from(values.get(key)?).ok()?;
    value
        .downcast_ref::<i32>()
        .ok()
        .or_else(|| {
            value
                .downcast_ref::<u32>()
                .ok()
                .and_then(|value| i32::try_from(value).ok())
        })
        .or_else(|| {
            value
                .downcast_ref::<f64>()
                .ok()
                .filter(|value| {
                    value.is_finite() && *value >= i32::MIN as f64 && *value <= i32::MAX as f64
                })
                .map(|value| value.round() as i32)
        })
}

fn terminal_records() -> Result<Vec<TabRecord>, String> {
    let connection = session_connection()?;
    let dbus = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .map_err(|err| err.to_string())?;
    let names: Vec<String> = dbus.call("ListNames", &()).map_err(|err| err.to_string())?;
    let mut records = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    for name in names
        .into_iter()
        .filter(|name| {
            name == "org.xfce.Terminal5" || name.starts_with("org.xfce.Terminal5.Instance.")
        })
        .take(128)
    {
        if Instant::now() >= deadline {
            return Err("Terminal metadata inspection timed out; no windows were changed".into());
        }
        let owner: u32 = dbus
            .call("GetConnectionUnixProcessID", &(name.as_str(),))
            .map_err(|err| err.to_string())?;
        let proxy = zbus::blocking::Proxy::new(
            &connection,
            name.as_str(),
            "/org/xfce/Terminal",
            "org.xfce.Terminal5",
        )
        .map_err(|err| err.to_string())?;
        let Ok(rows) = proxy
            .call::<_, _, Vec<HashMap<String, zbus::zvariant::OwnedValue>>>("ListTerminals", &())
        else {
            continue;
        };
        if rows.len() > MAX_WINDOWS * MAX_TABS {
            return Err("Too much terminal metadata".into());
        }
        for row in rows {
            // Retain incomplete tabs rather than silently dropping them: their
            // entire window will be unavailable, without blocking other servers.
            let active = row
                .get("active")
                .and_then(|value| zbus::zvariant::Value::try_from(value).ok())
                .and_then(|value| value.downcast_ref::<bool>().ok())
                .unwrap_or(false);
            records.push(TabRecord {
                owner: i32::try_from(owner).map_err(|err| err.to_string())?,
                window_uuid: value_string(&row, "window_uuid").unwrap_or_default(),
                tab_uuid: value_string(&row, "tab_uuid").unwrap_or_default(),
                title: value_string(&row, "window_title").unwrap_or_default(),
                cwd: value_string(&row, "working_directory").unwrap_or_default(),
                active,
                child: value_i32(&row, "child_pid").unwrap_or(0),
                foreground: value_i32(&row, "foreground_pgid").unwrap_or(0),
                pty: value_string(&row, "pty").unwrap_or_default(),
            });
        }
        if records.len() > MAX_WINDOWS * MAX_TABS {
            return Err("Too many terminal tabs".into());
        }
    }
    let mut seen = HashSet::new();
    records.retain(|record| {
        record.tab_uuid.is_empty() || seen.insert((record.owner, record.tab_uuid.clone()))
    });
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn target(id: &str, kind: &str) -> (UpgradeTarget, Directory) {
        let directory = Directory(std::env::temp_dir().join(format!(
            "terminal-upgrade-test-{}-{}",
            std::process::id(),
            crate::observability::next_operation_id()
        )));
        std::fs::create_dir(&directory.0).unwrap();
        let path = directory.0.join("fixture-only-not-executable");
        std::fs::write(&path, b"test fixture, never executed").unwrap();
        let file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let process = ProcessIdentity {
            pid: 123,
            name: "codex".into(),
            parent: 122,
            group: 123,
            start: 456,
            executable: "/fixture/codex".into(),
            arguments: vec!["codex".into()],
        };
        let record = TabRecord {
            owner: 122,
            window_uuid: "window-uuid".into(),
            tab_uuid: "tab-uuid".into(),
            title: "codex - ⠋ /fixture - Terminal".into(),
            cwd: "/fixture".into(),
            active: true,
            child: 123,
            foreground: 123,
            pty: "/dev/pts/fixture".into(),
        };
        let target = UpgradeTarget {
            window: TrackedWindow {
                id: id.into(),
                title: format!("{kind} - Terminal"),
                pid: 122,
                class: "xfce4-terminal".into(),
                x: 123,
                y: 234,
                width: 987,
                height: 654,
                stacking_order: 7,
                ..Default::default()
            },
            owner: process.clone(),
            tabs: vec![CapturedTab {
                record,
                process,
                busy: false,
                restore: TerminalTabRestore {
                    kind: kind.into(),
                    cwd: "/fixture".into(),
                    active: true,
                    ..Default::default()
                },
            }],
            executable: Arc::new(PinnedExecutable {
                file,
                path,
                device: metadata.dev(),
                inode: metadata.ino(),
                length: metadata.len(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                digest: "fixture digest".into(),
                build_id: Some("fixture build ID".into()),
            }),
        };
        (target, directory)
    }

    #[derive(Default)]
    struct FakeBackend {
        events: RefCell<Vec<String>>,
        fail: Option<&'static str>,
        windows: Vec<TrackedWindow>,
    }
    impl FakeBackend {
        fn event(&self, event: &str) -> Result<(), String> {
            self.events.borrow_mut().push(event.into());
            if self.fail == Some(event) {
                Err(format!("injected {event} failure"))
            } else {
                Ok(())
            }
        }
    }
    impl UpgradeBackend for FakeBackend {
        fn windows(&self) -> Vec<TrackedWindow> {
            self.windows.clone()
        }
        fn backup(&self, _: &[TrackedWindow], _: &[UpgradeTarget]) -> Result<i64, String> {
            self.event("backup")?;
            Ok(42)
        }
        fn preflight(&self, _: &UpgradeTarget) -> Result<(), String> {
            self.event("preflight")
        }
        fn launch_and_verify(&self, _: &UpgradeTarget) -> Result<String, String> {
            self.event("launch-and-verify")?;
            Ok("replacement".into())
        }
        fn close_and_wait(&self, _: &UpgradeTarget) -> Result<(), String> {
            self.event("close-one-window")
        }
        fn finish_layout(&self, _: &[(TrackedWindow, String)]) -> Result<(), String> {
            self.event("restore-stacking")
        }
    }

    fn ready_registry(targets: Vec<UpgradeTarget>) -> (UpgradeRegistry, String) {
        let mut registry = UpgradeRegistry::default();
        let (id, preparation) = registry.begin();
        let mut job = registry.jobs.back().unwrap().1.lock().unwrap();
        job.report.phase = TerminalUpgradePhase::Ready;
        job.report.windows = targets
            .iter()
            .map(|target| TerminalUpgradeWindow {
                window_id: target.window.id.clone(),
                title: target.window.title.clone(),
                tabs: vec!["fixture tab".into()],
                eligible: true,
                recommended: true,
                preserves_processes: target.preserves_processes(),
                executable: "/fixture/xfce4-terminal".into(),
                build_id: None,
                sha256: "fixture".into(),
                status: "ready".into(),
                message: "fixture preview".into(),
            })
            .collect();
        job.prepared = Some(PreparedUpgrade {
            all_windows: targets.iter().map(|target| target.window.clone()).collect(),
            targets,
        });
        drop(job);
        drop(preparation);
        (registry, id)
    }

    #[test]
    fn terminal_upgrade_requires_confirmation_and_supported_nonempty_unique_selection() {
        let (target, _directory) = target("one", "codex");
        let (registry, id) = ready_registry(vec![target]);
        assert!(registry.start(&id, &["one".into()], false).is_err());
        assert!(registry.start(&id, &[], true).is_err());
        assert!(registry.start(&id, &["other".into()], true).is_err());
        assert!(
            registry
                .start(&id, &["one".into(), "one".into()], true)
                .is_err()
        );
        assert_eq!(
            registry.report(&id).unwrap().phase,
            TerminalUpgradePhase::Ready
        );
        registry
            .jobs
            .back()
            .unwrap()
            .1
            .lock()
            .unwrap()
            .report
            .windows[0]
            .eligible = false;
        assert!(registry.start(&id, &["one".into()], true).is_err());
    }

    #[test]
    fn terminal_upgrade_start_is_single_use_and_does_not_duplicate_inflight_jobs() {
        let (target, _directory) = target("one", "codex");
        let (mut registry, id) = ready_registry(vec![target]);
        let work = registry.start(&id, &["one".into()], true).unwrap().unwrap();
        assert!(
            registry
                .start(&id, &["one".into()], true)
                .unwrap()
                .is_none()
        );
        let (same, preparation) = registry.begin();
        assert_eq!(same, id);
        assert!(preparation.is_none());
        drop(work);
        assert_eq!(
            registry.report(&id).unwrap().phase,
            TerminalUpgradePhase::Failed
        );
        assert!(registry.start(&id, &["one".into()], true).is_err());
    }

    #[test]
    fn expired_and_superseded_previews_cannot_close_windows() {
        let (target, _directory) = target("one", "codex");
        let (mut registry, id) = ready_registry(vec![target]);
        registry.jobs.back().unwrap().1.lock().unwrap().created = Instant::now() - PREVIEW_LIFETIME;
        assert!(registry.start(&id, &["one".into()], true).is_err());
        let (_, next) = registry.begin();
        assert!(
            registry
                .jobs
                .front()
                .unwrap()
                .1
                .lock()
                .unwrap()
                .prepared
                .is_none()
        );
        assert!(registry.start(&id, &["one".into()], true).is_err());
        drop(next);
    }

    #[test]
    fn preview_and_report_storage_is_bounded_and_never_runs_a_restart() {
        let mut registry = UpgradeRegistry::default();
        for _ in 0..MAX_JOBS * 3 {
            let (_, preparation) = registry.begin();
            drop(preparation);
            assert!(registry.jobs.len() <= MAX_JOBS);
            assert!(
                registry
                    .jobs
                    .iter()
                    .all(|(_, job)| job.lock().unwrap().prepared.is_none())
            );
        }
    }

    #[test]
    fn tmux_replacement_is_verified_before_original_is_closed() {
        let (target, _directory) = target("one", "tmux");
        let backend = FakeBackend::default();
        assert_eq!(replace_one(&backend, &target).unwrap(), "replacement");
        assert_eq!(
            *backend.events.borrow(),
            [
                "preflight",
                "launch-and-verify",
                "preflight",
                "close-one-window"
            ]
        );
        let failed = FakeBackend {
            fail: Some("launch-and-verify"),
            ..Default::default()
        };
        assert!(replace_one(&failed, &target).is_err());
        assert!(
            !failed
                .events
                .borrow()
                .iter()
                .any(|event| event == "close-one-window")
        );
    }

    #[test]
    fn non_tmux_original_closes_before_exact_session_is_resumed() {
        let (target, _directory) = target("one", "codex");
        let backend = FakeBackend::default();
        assert!(replace_one(&backend, &target).is_ok());
        assert_eq!(
            *backend.events.borrow(),
            ["preflight", "close-one-window", "launch-and-verify"]
        );
        let rejected = FakeBackend {
            fail: Some("close-one-window"),
            ..Default::default()
        };
        assert!(replace_one(&rejected, &target).is_err());
        assert!(
            !rejected
                .events
                .borrow()
                .iter()
                .any(|event| event == "launch-and-verify")
        );
    }

    #[test]
    fn non_tmux_restart_requires_the_original_process_instance_to_have_exited() {
        let stat = |state: &str, start: u64| {
            let mut fields = vec!["0".to_string(); 20];
            fields[0] = state.to_string();
            fields[19] = start.to_string();
            format!("123 (program with spaces) {}", fields.join(" "))
        };
        assert_eq!(
            exited_process_instance(stat("S", 456).as_bytes(), 456),
            Some(false)
        );
        assert_eq!(
            exited_process_instance(stat("R", 456).as_bytes(), 456),
            Some(false)
        );
        assert_eq!(
            exited_process_instance(stat("Z", 456).as_bytes(), 456),
            Some(true)
        );
        assert_eq!(
            exited_process_instance(stat("S", 789).as_bytes(), 456),
            Some(true)
        );
        assert_eq!(exited_process_instance(b"incomplete metadata", 456), None);
    }

    #[test]
    fn backup_failure_prevents_every_close_and_launch() {
        let (target, _directory) = target("one", "codex");
        let (registry, id) = ready_registry(vec![target]);
        let backend = FakeBackend {
            fail: Some("backup"),
            ..Default::default()
        };
        registry
            .start(&id, &["one".into()], true)
            .unwrap()
            .unwrap()
            .run(&backend);
        assert_eq!(*backend.events.borrow(), ["backup"]);
        let report = registry.report(&id).unwrap();
        assert_eq!(report.phase, TerminalUpgradePhase::Failed);
        assert!(report.backup_snapshot_id.is_none());
    }

    #[test]
    fn failures_do_not_force_kill_or_retry_and_saved_backup_survives() {
        for failure in ["preflight", "close-one-window", "launch-and-verify"] {
            let (target, _directory) = target("one", "codex");
            let (registry, id) = ready_registry(vec![target]);
            let backend = FakeBackend {
                fail: Some(failure),
                ..Default::default()
            };
            registry
                .start(&id, &["one".into()], true)
                .unwrap()
                .unwrap()
                .run(&backend);
            let report = registry.report(&id).unwrap();
            assert_eq!(report.backup_snapshot_id, Some(42));
            assert_eq!(report.windows[0].status, "failed");
            for event in ["close-one-window", "launch-and-verify"] {
                assert!(
                    backend
                        .events
                        .borrow()
                        .iter()
                        .filter(|actual| *actual == event)
                        .count()
                        <= 1
                );
            }
            assert!(
                !backend
                    .events
                    .borrow()
                    .iter()
                    .any(|event| event.contains("kill"))
            );
            assert!(
                !backend
                    .events
                    .borrow()
                    .iter()
                    .any(|event| event == "restore-stacking"),
                "failed replacements must not reorder or activate unrelated windows"
            );
        }
    }

    #[test]
    fn successful_job_is_backed_up_first_and_complete_reports_cannot_replay_it() {
        let (target, _directory) = target("one", "codex");
        let original = target.window.clone();
        let (registry, id) = ready_registry(vec![target]);
        let backend = FakeBackend {
            windows: vec![original],
            ..Default::default()
        };
        registry
            .start(&id, &["one".into()], true)
            .unwrap()
            .unwrap()
            .run(&backend);
        assert_eq!(backend.events.borrow().first().unwrap(), "backup");
        assert_eq!(registry.report(&id).unwrap().windows[0].status, "restarted");
        assert_eq!(
            registry.report(&id).unwrap().phase,
            TerminalUpgradePhase::Complete
        );
        assert!(
            registry
                .start(&id, &["one".into()], true)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            backend
                .events
                .borrow()
                .iter()
                .filter(|event| *event == "close-one-window")
                .count(),
            1
        );
    }

    #[test]
    fn mixed_tab_window_is_not_mistaken_for_a_lossless_tmux_window() {
        let (mut target, _directory) = target("one", "tmux");
        let mut ordinary = target.tabs[0].clone();
        ordinary.restore.kind = "codex".into();
        ordinary.restore.active = false;
        target.tabs.push(ordinary);
        assert!(!target.preserves_processes());
        assert_eq!(target.restore_tabs().len(), 2);
    }

    #[test]
    fn incomplete_tab_metadata_rejects_the_whole_window_instead_of_dropping_a_tab() {
        let (target, _directory) = target("one", "codex");
        let original = target.tabs[0].record.clone();
        let mut incomplete = original.clone();
        incomplete.active = false;
        incomplete.tab_uuid = "incomplete-tab".into();
        incomplete.child = 0;
        let error = capture_tabs(&target.window, &[original.clone(), incomplete])
            .err()
            .unwrap();
        assert!(error.contains("entire window is skipped"));
        let mut missing_window = original;
        missing_window.window_uuid.clear();
        assert!(
            capture_tabs(&target.window, &[missing_window])
                .err()
                .unwrap()
                .contains("incomplete window UUIDs")
        );
        // Both errors occur before any /proc lookup or desktop interaction.
    }

    #[test]
    fn known_terminal_program_aliases_are_recognised_without_replaying_wrapper_arguments() {
        let (target, _directory) = target("one", "codex");
        let mut process = target.tabs[0].process.clone();
        process.name = "agy".into();
        process.executable = "/fixture/node".into();
        assert_eq!(program_kind(&process), "agy");
        process.name = "codex-code-mode".into();
        assert_eq!(program_kind(&process), "codex");
    }

    #[test]
    fn changed_binary_invalidates_preview_while_original_artifact_stays_pinned() {
        let (target, directory) = target("one", "codex");
        assert!(target.executable.validate_current().is_ok());
        let replacement = directory.0.join("replacement-fixture");
        std::fs::write(&replacement, b"new fixture").unwrap();
        std::fs::rename(replacement, &target.executable.path).unwrap();
        assert!(target.executable.validate_current().is_err());
        assert_eq!(
            std::fs::read(target.executable.pinned_path()).unwrap(),
            b"test fixture, never executed"
        );
    }

    #[test]
    fn pinned_launch_preserves_program_identity_and_forwards_arguments_without_interpolation() {
        let (target, _directory) = target("one", "codex");
        let supplied = vec![
            std::ffi::OsString::from("--disable-server"),
            "/directory/with spaces; $(not-a-command)".into(),
        ];
        let command = pinned_launch_command(&target.executable, supplied.clone()).unwrap();
        let arguments = command.get_args().collect::<Vec<_>>();
        assert_eq!(arguments[0], "-c");
        assert_eq!(arguments[1], "exec -a \"$1\" \"$2\" \"${@:3}\"");
        assert_eq!(arguments[3], target.executable.path);
        assert_eq!(arguments[4], target.executable.pinned_path());
        assert_eq!(
            &arguments[5..],
            supplied
                .iter()
                .map(|argument| argument.as_os_str())
                .collect::<Vec<_>>()
        );
        // This test inspects argv only: it never executes the fixture or a terminal.
    }

    #[test]
    fn braille_frames_do_not_change_identity_but_tab_process_and_pty_changes_do() {
        let (target, _directory) = target("one", "codex");
        let original = &target.tabs[0].record;
        let mut spinner = original.clone();
        spinner.title = spinner.title.replace('⠋', "⠹");
        assert!(same_tab_identity(original, &spinner));
        for change in 0..5 {
            let mut changed = original.clone();
            match change {
                0 => changed.owner += 1,
                1 => changed.foreground += 1,
                2 => changed.pty.push('x'),
                3 => changed.tab_uuid.push('x'),
                _ => changed.active = false,
            }
            assert!(!same_tab_identity(original, &changed));
        }
    }

    #[test]
    fn movement_resize_and_state_changes_invalidate_saved_placement() {
        let (target, _directory) = target("one", "codex");
        assert!(same_placement(&target.window, &target.window));
        for change in 0..7 {
            let mut changed = target.window.clone();
            match change {
                0 => changed.x += 1,
                1 => changed.width += 1,
                2 => changed.minimized = true,
                3 => changed.maximized = true,
                4 => changed.fullscreen = true,
                5 => changed.desktop += 1,
                _ => changed.output.push('x'),
            }
            assert!(!same_placement(&target.window, &changed));
        }
    }

    #[test]
    fn exact_codex_id_comes_only_from_valid_session_metadata() {
        let id = "12345678-1234-5678-9abc-123456789abc";
        assert_eq!(
            rollout_session_id(&format!(
                r#"{{"type":"session_meta","payload":{{"id":"{id}"}}}}"#
            ))
            .as_deref(),
            Some(id)
        );
        for value in [
            r#"{"type":"event_msg","payload":{"id":"12345678-1234-5678-9abc-123456789abc"}}"#,
            r#"{"type":"session_meta","payload":{"id":"--last"}}"#,
            r#"{"type":"session_meta","payload":{"id":"x; rm -rf /"}}"#,
            "not json",
        ] {
            assert_eq!(rollout_session_id(value), None);
        }
    }
}
